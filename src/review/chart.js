// Hover tooltips for the charts on the sensors page. The charts, their
// legends and the tables under them work without this script. It adds a
// crosshair that snaps to the nearest point and a tooltip with every
// sensor's value there, and any faulty readings left out of it, following
// the pointer, or the arrow keys once a chart has focus. A chart that zooms
// opens the period one step in where it is clicked, or at the point shown
// when Enter is pressed; its fault marks and times are links of their own.
// Hovering or focusing a fault mark draws its faulty readings into the
// chart as a dashed red line through their values, from the line's point
// before them to the one after, stretching the chart's scale while it shows
// when they lie off it.
//
// A page with data-refresh on its body, the live site's newest period,
// reloads itself every so many seconds to show new readings, but waits
// while a chart is pointed at or has focus, or the page is hidden.
(function () {
    "use strict";

    // Pixels between the crosshair and the tooltip.
    var GAP = 10;

    // The date in a chart's data-zoom to replace with the one to zoom to.
    var ZOOM_DATE = "0001-01-01";

    var SVG = "http://www.w3.org/2000/svg";

    // Round values for the side of a chart from at or below `low` to at or
    // above `high`, about five, as the server picks them, and how many
    // decimals they need.
    function valueTicks(low, high) {
        if (high - low < 1e-9) {
            low -= 1;
            high += 1;
        }
        var rough = (high - low) / 5;
        var magnitude = Math.pow(10, Math.floor(Math.log10(rough)));
        var step = 10 * magnitude;
        [1, 2, 2.5, 5, 10].some(function (multiple) {
            if (multiple * magnitude >= rough * (1 - 1e-9)) {
                step = multiple * magnitude;
                return true;
            }
            return false;
        });
        var values = [];
        for (var n = Math.floor(low / step); n <= Math.ceil(high / step); n++) {
            values.push(n * step);
        }
        var decimals = 0;
        while (decimals < 6) {
            var scaled = step * Math.pow(10, decimals);
            if (Math.abs(scaled - Math.round(scaled)) < 1e-6) {
                break;
            }
            decimals++;
        }
        return { values: values, decimals: decimals };
    }

    // The date of a time, as the logs show times, like YYYY-MM-DD.
    function dateOf(at) {
        return new Date(at).toISOString().slice(0, 10);
    }

    function format(value, unit) {
        if (value === null || value === undefined) {
            return "-";
        }
        var text = value.toLocaleString(undefined, { maximumFractionDigits: 2 });
        return unit ? text + " " + unit : text;
    }

    // The point nearest `index` where a sensor has a reading, looking no
    // further across the plot than `near`, or -1. Without `near`, only
    // `index` itself.
    function readingNear(series, xs, index, near) {
        if (series.mean[index] !== null) {
            return index;
        }
        for (var step = 1; near > 0; step++) {
            var before = index - step;
            var after = index + step;
            var beforeClose = before >= 0 && xs[index] - xs[before] <= near;
            var afterClose = after < xs.length && xs[after] - xs[index] <= near;
            if (!beforeClose && !afterClose) {
                break;
            }
            if (beforeClose && series.mean[before] !== null) {
                return before;
            }
            if (afterClose && series.mean[after] !== null) {
                return after;
            }
        }
        return -1;
    }

    // The index of the point closest to a place across the plot, from 0 to 1.
    function nearest(xs, fraction) {
        var low = 0;
        var high = xs.length - 1;
        while (low < high) {
            var middle = (low + high) >> 1;
            if (xs[middle] < fraction) {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if (low > 0 && fraction - xs[low - 1] < xs[low] - fraction) {
            return low - 1;
        }
        return low;
    }

    function element(tag, className, text) {
        var node = document.createElement(tag);
        if (className) {
            node.className = className;
        }
        if (text !== undefined) {
            // Names come from the logs, so never as HTML.
            node.textContent = text;
        }
        return node;
    }

    function setUp(figure) {
        var data;
        try {
            data = JSON.parse(figure.dataset.points);
        } catch (error) {
            return;
        }
        var plot = figure.querySelector(".plot");
        if (!plot || !data.x || data.x.length === 0) {
            return;
        }
        // The tooltip says what a fault mark's title does, so the browser
        // need not show its own as well.
        figure.querySelectorAll(".fault[title]").forEach(function (mark) {
            mark.setAttribute("aria-label", mark.title);
            mark.removeAttribute("title");
        });
        var crosshair = element("div", "crosshair");
        var tooltip = element("div", "chart-tooltip");
        crosshair.hidden = true;
        tooltip.hidden = true;
        tooltip.setAttribute("aria-live", "polite");
        plot.append(crosshair, tooltip);
        // Focusable only with the script, since only it does anything then.
        plot.tabIndex = 0;
        var current = data.x.length - 1;
        var zoom = figure.dataset.zoom;
        var start = Number(figure.dataset.start);
        var end = Number(figure.dataset.end);

        // Opens the period one step in that holds a place across the plot.
        function zoomTo(fraction) {
            if (zoom) {
                window.location.href = zoom.replace(ZOOM_DATE, dateOf(start + fraction * (end - start)));
            }
        }

        function show(index) {
            current = index;
            var left = data.x[index] * 100;
            crosshair.style.left = left + "%";
            crosshair.hidden = false;

            tooltip.replaceChildren(
                element("div", "tooltip-time", data.times[index] + " · " + data.every)
            );
            data.series.forEach(function (series) {
                // Each sensor's own reading nearest the crosshair, so every
                // sensor stays in the tooltip in a close-up.
                var found = readingNear(series, data.x, index, data.near || 0);
                if (found >= 0) {
                    var row = element("div", "tooltip-row");
                    row.append(
                        element("span", "line-key s" + series.slot),
                        element("strong", "", format(series.mean[found], data.unit)),
                        element("span", "muted", series.name)
                    );
                    var low = series.low[found];
                    var high = series.high[found];
                    if (low !== null && high !== null && low !== high) {
                        row.append(element("span", "muted", format(low) + " to " + format(high)));
                    }
                    if (data.near) {
                        // Its own time, to the second.
                        row.append(element("span", "muted", data.times[found].slice(11)));
                    }
                    tooltip.append(row);
                }
                var faults = series.faults && series.faults[index];
                if (faults) {
                    var fault = element("div", "tooltip-row");
                    fault.append(
                        element("span", "fault-key"),
                        element("span", "", series.name + ", left out: " + faults)
                    );
                    tooltip.append(fault);
                }
            });
            if (zoom) {
                tooltip.append(element("div", "tooltip-hint", "Click to zoom in"));
            }
            // Right of the crosshair, or left of it near the right edge, and
            // never past the chart's edges, which may reach over the values.
            tooltip.hidden = false;
            var width = plot.clientWidth;
            var at = data.x[index] * width;
            var place = at + GAP;
            if (place + tooltip.offsetWidth > width) {
                place = at - GAP - tooltip.offsetWidth;
            }
            place = Math.max(-plot.offsetLeft, Math.min(place, width - tooltip.offsetWidth));
            tooltip.style.left = place + "px";
        }

        function hide() {
            crosshair.hidden = true;
            tooltip.hidden = true;
        }

        plot.addEventListener("pointermove", function (event) {
            var box = plot.getBoundingClientRect();
            if (box.width > 0) {
                show(nearest(data.x, (event.clientX - box.left) / box.width));
            }
        });
        plot.addEventListener("pointerleave", hide);
        plot.addEventListener("click", function (event) {
            // Fault marks link to close-ups of their own.
            if (event.target.closest("a")) {
                return;
            }
            var box = plot.getBoundingClientRect();
            if (box.width > 0) {
                zoomTo((event.clientX - box.left) / box.width);
            }
        });
        plot.addEventListener("focus", function () {
            show(current);
        });
        plot.addEventListener("blur", hide);
        // Drawing a fault mark's readings into the chart, and back.
        var svg = plot.querySelector("svg");
        var lines = svg && svg.querySelector("g.data");
        var yAxis = figure.querySelector(".y-axis");
        var drawn = [];

        function unpreview() {
            drawn.forEach(function (node) {
                node.remove();
            });
            drawn = [];
            if (lines) {
                lines.removeAttribute("transform");
            }
            figure.classList.remove("previewing", "rescaled");
        }

        function preview(mark) {
            unpreview();
            var series = data.series[Number(mark.dataset.line)];
            var faults;
            try {
                faults = JSON.parse(mark.dataset.faults);
            } catch (error) {
                return;
            }
            if (!svg || !lines || !series || !faults.length) {
                return;
            }
            var values = faults.map(function (fault) {
                return fault[1];
            });
            var low = Math.min(data.low, Math.min.apply(null, values));
            var high = Math.max(data.high, Math.max.apply(null, values));
            if (low < data.low || high > data.high) {
                // Stretch the lines to a scale that holds the faults too,
                // with grid lines and values for it.
                var ticks = valueTicks(low, high);
                low = ticks.values[0];
                high = ticks.values[ticks.values.length - 1];
                var k = (data.high - data.low) / (high - low);
                var b = 1000 * (1 - k) - ((data.low - low) * 1000) / (high - low);
                lines.setAttribute("transform", "matrix(1 0 0 " + k + " 0 " + b + ")");
                ticks.values.forEach(function (value) {
                    var top = (1 - (value - low) / (high - low)) * 100;
                    var grid = document.createElementNS(SVG, "line");
                    grid.setAttribute("class", "grid preview");
                    grid.setAttribute("x1", "0");
                    grid.setAttribute("x2", "1000");
                    grid.setAttribute("y1", String(top * 10));
                    grid.setAttribute("y2", String(top * 10));
                    svg.insertBefore(grid, svg.firstChild);
                    var label = element("span", "preview-tick", (value + 0).toFixed(ticks.decimals));
                    label.style.top = top + "%";
                    yAxis.append(label);
                    drawn.push(grid, label);
                });
                figure.classList.add("rescaled");
            }
            var y = function (value) {
                return 1000 - ((value - low) / (high - low)) * 1000;
            };
            // From the line's point just before the faults to the one after.
            var first = faults[0][0];
            var last = faults[faults.length - 1][0];
            var before = -1;
            var after = -1;
            for (var i = 0; i < data.x.length && after < 0; i++) {
                if (series.mean[i] === null) {
                    continue;
                }
                if (data.x[i] <= first) {
                    before = i;
                } else if (data.x[i] >= last) {
                    after = i;
                }
            }
            var points = [];
            if (before >= 0) {
                points.push([data.x[before], series.mean[before]]);
            }
            points = points.concat(faults);
            if (after >= 0) {
                points.push([data.x[after], series.mean[after]]);
            }
            var path = document.createElementNS(SVG, "path");
            path.setAttribute("class", "preview-line");
            path.setAttribute(
                "d",
                points
                    .map(function (point, index) {
                        return (index ? "L" : "M") + (point[0] * 1000).toFixed(1) + "," + y(point[1]).toFixed(1);
                    })
                    .join(" ")
            );
            svg.append(path);
            drawn.push(path);
            faults.forEach(function (fault) {
                var dot = element("span", "preview-dot");
                dot.style.left = fault[0] * 100 + "%";
                dot.style.top = y(fault[1]) / 10 + "%";
                plot.append(dot);
                drawn.push(dot);
            });
            figure.classList.add("previewing");
        }

        figure.querySelectorAll(".fault[data-faults]").forEach(function (mark) {
            mark.addEventListener("pointerenter", function () {
                preview(mark);
            });
            mark.addEventListener("pointerleave", unpreview);
            mark.addEventListener("focus", function () {
                preview(mark);
            });
            mark.addEventListener("blur", unpreview);
        });

        plot.addEventListener("keydown", function (event) {
            var last = data.x.length - 1;
            var moves = {
                ArrowLeft: current - 1,
                ArrowRight: current + 1,
                Home: 0,
                End: last
            };
            if (event.key in moves) {
                show(Math.min(last, Math.max(0, moves[event.key])));
                event.preventDefault();
            } else if (event.key === "Enter") {
                zoomTo(data.x[current]);
            } else if (event.key === "Escape") {
                hide();
            }
        });
    }

    document.querySelectorAll("figure.chart[data-points]").forEach(setUp);

    var refresh = Number(document.body.dataset.refresh);
    if (refresh > 0) {
        var due = Date.now() + refresh * 1000;
        window.setInterval(function () {
            var busy = document.hidden || document.querySelector(".plot:hover, .plot:focus-within");
            if (Date.now() >= due && !busy) {
                window.location.reload();
            }
        }, 1000);
    }
})();
