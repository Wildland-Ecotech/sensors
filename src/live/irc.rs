//! Keeps `sensors live` in its channels: connects over TLS, joins them,
//! rejoins them when needed, takes its nick back when it was taken,
//! reconnects after a lost connection, and quits with a message when it is
//! stopped. Along the way it logs what is said in its channels, and who
//! joins, leaves or is kicked, as irssi would, and asks the analysis to read
//! the new lines. It never says anything itself.
//!
//! The connection comes from lurker, the bot in wetfish/lurkers.

use std::collections::HashSet;
use std::error::Error;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use irc::client::ClientStream;
use irc::client::prelude::{Client, Command, Config as IrcConfig, Message, Prefix, Response};
use tokio::time::{self, Instant, MissedTickBehavior};

use super::health::Status;
use super::log::{Logs, fold};
use super::shutdown::Shutdown;
use super::{Analysis, Settings};

/// Sent with QUIT when `sensors live` is stopped, which is usually for a
/// deploy.
const QUIT_MESSAGE: &str = "brb, getting a software update";

/// How long connecting, including the TLS handshake, may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for the server to close the connection after QUIT.
const QUIT_TIMEOUT: Duration = Duration::from_secs(5);

/// How often to join the channels it is not in, such as after a kick.
const REJOIN_INTERVAL: Duration = Duration::from_secs(60);

/// The wait before reconnecting, doubled after each attempt up to
/// `MAX_RETRY_DELAY`.
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(5);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(300);

/// A connection that lasted this long counts as having worked, so the wait
/// before reconnecting starts again from `FIRST_RETRY_DELAY`.
const STABLE_CONNECTION: Duration = Duration::from_secs(300);

/// Stays connected, reconnecting whenever the connection is lost, until
/// stopped, then closes the logs.
pub async fn run(
    settings: &Settings,
    logs: &mut Logs,
    analysis: &Analysis,
    status: &Status,
    shutdown: &mut Shutdown,
) {
    let mut delay = FIRST_RETRY_DELAY;
    loop {
        let started = Instant::now();
        let result = connect(settings, logs, analysis, status, shutdown).await;
        status.set_connected(false);
        match result {
            Ok(()) => break,
            Err(err) => eprintln!("error: {err}"),
        }

        if started.elapsed() >= STABLE_CONNECTION {
            delay = FIRST_RETRY_DELAY;
        }
        eprintln!("Reconnecting in {} seconds", delay.as_secs());
        tokio::select! {
            () = time::sleep(delay) => {}
            () = shutdown.recv() => break,
        }
        delay = (delay * 2).min(MAX_RETRY_DELAY);
    }
    logs.close_all(now());
}

/// Connects once and logs the channels until the connection is lost, which
/// is an error, or it is stopped.
async fn connect(
    settings: &Settings,
    logs: &mut Logs,
    analysis: &Analysis,
    status: &Status,
    shutdown: &mut Shutdown,
) -> Result<(), String> {
    let server = &settings.server;
    eprintln!("Connecting to {server}:{}", settings.port);
    let connecting = time::timeout(CONNECT_TIMEOUT, Client::from_config(irc_config(settings)));
    let mut client = tokio::select! {
        result = connecting => match result {
            Ok(Ok(client)) => client,
            Ok(Err(err)) => return Err(format!("connecting to {server}: {}", describe(&err))),
            Err(_) => {
                return Err(format!(
                    "connecting to {server}: no answer after {} seconds",
                    CONNECT_TIMEOUT.as_secs()
                ));
            }
        },
        () = shutdown.recv() => return Ok(()),
    };
    client.identify().map_err(|err| describe(&err))?;
    let mut stream = client.stream().map_err(|err| describe(&err))?;
    let mut session = Session::new(settings);
    serve(
        &client,
        &mut stream,
        &mut session,
        logs,
        analysis,
        status,
        shutdown,
    )
    .await
}

/// Handles messages and rejoins channels until the connection is lost or it
/// is stopped.
async fn serve(
    client: &Client,
    stream: &mut ClientStream,
    session: &mut Session<'_>,
    logs: &mut Logs,
    analysis: &Analysis,
    status: &Status,
    shutdown: &mut Shutdown,
) -> Result<(), String> {
    let mut rejoin = time::interval(REJOIN_INTERVAL);
    rejoin.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let commands = tokio::select! {
            message = stream.next() => match message {
                Some(Ok(message)) => session.handle(&message, logs, now()),
                Some(Err(err)) => return Err(format!("connection lost: {}", describe(&err))),
                None => return Err("the server closed the connection".to_owned()),
            },
            _ = rejoin.tick() => session.rejoin(),
            () = shutdown.recv() => {
                quit(client, stream).await;
                return Ok(());
            }
        };
        if std::mem::take(&mut session.logged) {
            analysis.ask();
        }
        for command in commands {
            client.send(command).map_err(|err| describe(&err))?;
        }
        status.set_connected(session.in_every_channel());
    }
}

/// Sends QUIT, then keeps reading until the server closes the connection,
/// since that is what writes the QUIT out.
async fn quit(client: &Client, stream: &mut ClientStream) {
    eprintln!("Quitting: {QUIT_MESSAGE}");
    if client.send_quit(QUIT_MESSAGE).is_err() {
        return;
    }
    let closed = async { while let Some(Ok(_)) = stream.next().await {} };
    let _ = time::timeout(QUIT_TIMEOUT, closed).await;
}

/// The irc crate's settings for one connection. The crate joins the
/// channels once the server has accepted the connection, answers server
/// pings and CTCP requests, and tries the alternative nicks when the nick is
/// taken.
fn irc_config(settings: &Settings) -> IrcConfig {
    let nick = &settings.nick;
    IrcConfig {
        nickname: Some(nick.clone()),
        alt_nicks: vec![format!("{nick}_"), format!("{nick}__")],
        username: Some(nick.clone()),
        realname: Some("sensors, graphing readings posted here".to_owned()),
        server: Some(settings.server.clone()),
        port: Some(settings.port),
        use_tls: Some(true),
        channels: settings.channels.clone(),
        version: Some(format!("sensors {}", env!("CARGO_PKG_VERSION"))),
        source: Some(env!("CARGO_PKG_REPOSITORY").to_owned()),
        ..IrcConfig::default()
    }
}

/// An error and its causes, since the irc crate's own messages, such as
/// "an io error occurred", leave the cause to its source.
fn describe(err: &dyn Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn now() -> DateTime<Utc> {
    DateTime::from(SystemTime::now())
}

/// A message's `ident@host`, as irssi shows it in joins and parts.
fn address(message: &Message) -> String {
    match &message.prefix {
        Some(Prefix::Nickname(_, user, host)) => format!("{user}@{host}"),
        _ => String::new(),
    }
}

/// What one connection knows.
struct Session<'a> {
    settings: &'a Settings,
    /// The nick the server knows it by, which is another while its own is
    /// taken.
    nick: String,
    /// Whether the server has accepted the connection.
    registered: bool,
    /// The channels it is in, folded.
    joined: HashSet<String>,
    /// Whether lines were logged since the analysis was last asked to read
    /// them.
    logged: bool,
}

impl<'a> Session<'a> {
    fn new(settings: &'a Settings) -> Session<'a> {
        Session {
            settings,
            nick: settings.nick.clone(),
            registered: false,
            joined: HashSet::new(),
            logged: false,
        }
    }

    /// Logs what a message says happened in a channel, and returns the
    /// commands to send in reply.
    fn handle(&mut self, message: &Message, logs: &mut Logs, now: DateTime<Utc>) -> Vec<Command> {
        let source = message.source_nickname().unwrap_or("");
        let mut commands = Vec::new();
        match &message.command {
            Command::Response(Response::RPL_WELCOME, args) => {
                if let Some(nick) = args.first() {
                    self.nick = nick.clone();
                }
                self.registered = true;
                eprintln!("Connected as {}", self.nick);
            }

            Command::JOIN(channels, _, _) => {
                for channel in self.wanted_in(channels) {
                    if self.is_me(source) {
                        self.joined.insert(fold(channel));
                        eprintln!("Joined {channel}");
                    }
                    let line = format!("-!- {source} [{}] has joined {channel}", address(message));
                    self.log(logs, channel, now, &line);
                }
            }
            Command::PART(channels, reason) => {
                for channel in self.wanted_in(channels) {
                    let line = format!(
                        "-!- {source} [{}] has left {channel} [{}]",
                        address(message),
                        reason.as_deref().unwrap_or("")
                    );
                    self.log(logs, channel, now, &line);
                    if self.is_me(source) {
                        self.joined.remove(&fold(channel));
                        eprintln!("Left {channel}, rejoining within a minute");
                    }
                }
            }
            Command::KICK(channels, target, reason) => {
                for channel in self.wanted_in(channels) {
                    let line = format!(
                        "-!- {target} was kicked from {channel} by {source} [{}]",
                        reason.as_deref().unwrap_or("")
                    );
                    self.log(logs, channel, now, &line);
                    if self.is_me(target) {
                        self.joined.remove(&fold(channel));
                        eprintln!("Kicked from {channel} by {source}, rejoining within a minute");
                    }
                }
            }
            Command::QUIT(_) => commands.extend(self.reclaim_nick(source)),
            Command::NICK(new_nick) if self.is_me(source) => {
                eprintln!("Now known as {new_nick}");
                self.nick = new_nick.clone();
            }
            Command::NICK(_) => commands.extend(self.reclaim_nick(source)),

            // What is said in a channel, including with /me, but not other
            // CTCP requests, which the irc crate answers
            Command::PRIVMSG(target, text) if self.is_wanted(target) => {
                if let Some(action) = text.strip_prefix("\u{1}ACTION ") {
                    let action = action.trim_end_matches('\u{1}');
                    self.log(logs, target, now, &format!(" * {source} {action}"));
                } else if !text.starts_with('\u{1}') {
                    self.log(logs, target, now, &format!("< {source}> {text}"));
                }
            }

            Command::ERROR(text) => eprintln!("Server error: {text}"),
            // Such as being unable to join a channel
            Command::Response(response, args) if (400..600).contains(&(*response as u16)) => {
                eprintln!("warning: {response:?} {}", args.join(" "));
            }
            _ => {}
        }
        commands
    }

    /// Writes a line to a channel's log.
    fn log(&mut self, logs: &mut Logs, channel: &str, now: DateTime<Utc>, line: &str) {
        match logs.write(channel, now, line) {
            Ok(()) => self.logged = true,
            Err(err) => eprintln!(
                "warning: writing to {}: {err}",
                logs.path(channel).display()
            ),
        }
    }

    /// Joins the channels it is not in, such as after a kick, once the
    /// server has accepted the connection.
    fn rejoin(&self) -> Vec<Command> {
        if !self.registered {
            return Vec::new();
        }
        self.settings
            .channels
            .iter()
            .filter(|channel| !self.joined.contains(&fold(channel)))
            .map(|channel| Command::JOIN(channel.clone(), None, None))
            .collect()
    }

    /// Whether it is on IRC and in every channel, so reading all it should.
    fn in_every_channel(&self) -> bool {
        self.registered
            && self
                .settings
                .channels
                .iter()
                .all(|channel| self.joined.contains(&fold(channel)))
    }

    /// Asks for its nick back when whoever had it quit or changed nick.
    fn reclaim_nick(&self, source: &str) -> Option<Command> {
        let nick = &self.settings.nick;
        if !source.eq_ignore_ascii_case(nick)
            || !self.registered
            || self.nick.eq_ignore_ascii_case(nick)
        {
            return None;
        }
        eprintln!("Taking back the nick {nick}");
        Some(Command::NICK(nick.clone()))
    }

    fn is_me(&self, nick: &str) -> bool {
        nick.eq_ignore_ascii_case(&self.nick)
    }

    /// The channels it logs in a comma-separated list, as a JOIN, PART or
    /// KICK has.
    fn wanted_in<'m>(&self, list: &'m str) -> Vec<&'m str> {
        list.split(',')
            .filter(|channel| self.is_wanted(channel))
            .collect()
    }

    fn is_wanted(&self, channel: &str) -> bool {
        self.settings
            .channels
            .iter()
            .any(|wanted| fold(wanted) == fold(channel))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::discover::tests::TempDir;

    fn settings(root: &TempDir) -> Settings {
        Settings {
            server: "irc.example.net".into(),
            port: 6697,
            nick: "sensors".into(),
            channels: vec!["#greenhouse".into()],
            network: "wetfish".into(),
            time_zone: "America/Denver".parse().unwrap(),
            logs: root.0.join("logs"),
            db: root.0.join("sensors.sqlite"),
            listen: "127.0.0.1:0".parse().unwrap(),
        }
    }

    fn message(line: &str) -> Message {
        line.parse().unwrap()
    }

    /// 18:03 in Denver.
    fn at() -> DateTime<Utc> {
        "2026-11-05T01:03:07Z".parse().unwrap()
    }

    #[test]
    fn logs_its_channels_as_irssi_does() {
        let root = TempDir::new("live-irc");
        let settings = settings(&root);
        let mut logs = Logs::new(&settings.logs, &settings.network, settings.time_zone).unwrap();
        let mut session = Session::new(&settings);
        let mut hear = |line: &str| session.handle(&message(line), &mut logs, at());

        assert!(hear(":irc.example.net 001 sensors_ :Welcome").is_empty());
        hear(":sensors_!sensors@Fish-1.IP JOIN #GreenHouse");
        hear(r#":greenhouse!pi@Fish-2.IP PRIVMSG #greenhouse :{"sensor":"inside","temp":"51.44"}"#);
        hear(":rachel!r@home PRIVMSG #greenhouse :\u{1}ACTION waters the plants\u{1}");
        hear(":rachel!r@home PRIVMSG #greenhouse :\u{1}VERSION\u{1}");
        hear(":rachel!r@home PRIVMSG #elsewhere :not logged");
        hear(":rachel!r@home PRIVMSG sensors_ :not logged either");
        hear(":greenhouse!pi@Fish-2.IP PART #greenhouse :reconnecting");
        hear(":rachel!r@home KICK #greenhouse sensors_ :testing");
        assert!(session.logged);

        let text = fs::read_to_string(root.0.join("logs/wetfish/#greenhouse.log")).unwrap();
        assert_eq!(
            text,
            "--- Log opened Wed Nov 04 18:03:07 2026\n\
             18:03 -!- sensors_ [sensors@Fish-1.IP] has joined #GreenHouse\n\
             18:03 < greenhouse> {\"sensor\":\"inside\",\"temp\":\"51.44\"}\n\
             18:03  * rachel waters the plants\n\
             18:03 -!- greenhouse [pi@Fish-2.IP] has left #greenhouse [reconnecting]\n\
             18:03 -!- sensors_ was kicked from #greenhouse by rachel [testing]\n"
        );
        assert!(!root.0.join("logs/wetfish/#elsewhere.log").exists());
    }

    #[test]
    fn rejoins_and_takes_its_nick_back() {
        let root = TempDir::new("live-irc-rejoin");
        let settings = settings(&root);
        let mut logs = Logs::new(&settings.logs, &settings.network, settings.time_zone).unwrap();
        let mut session = Session::new(&settings);

        assert!(session.rejoin().is_empty(), "not connected yet");
        session.handle(&message(":irc 001 sensors_ :Welcome"), &mut logs, at());
        assert_eq!(
            session.rejoin(),
            [Command::JOIN("#greenhouse".into(), None, None)]
        );
        assert!(!session.in_every_channel());

        session.handle(&message(":sensors_!s@h JOIN #greenhouse"), &mut logs, at());
        assert!(session.rejoin().is_empty());
        assert!(session.in_every_channel());

        // Whoever had the nick leaves it.
        let commands = session.handle(&message(":Sensors!x@y QUIT :bye"), &mut logs, at());
        assert_eq!(commands, [Command::NICK("sensors".into())]);
        session.handle(&message(":sensors_!s@h NICK sensors"), &mut logs, at());
        assert_eq!(session.nick, "sensors");
        // With its own nick, another leaving needs nothing.
        assert!(
            session
                .handle(&message(":sensors_!x@y QUIT :bye"), &mut logs, at())
                .is_empty()
        );

        session.handle(
            &message(":rachel!r@h KICK #greenhouse sensors :out"),
            &mut logs,
            at(),
        );
        assert!(!session.in_every_channel());
        assert_eq!(session.rejoin().len(), 1);
    }
}
