// Hover tooltips for the charts on the sensors page. The charts, their
// legends and the tables under them work without this script. It adds a
// crosshair that snaps to the nearest point and a tooltip with every
// sensor's value there, and any faulty readings left out of it, following
// the pointer, or the arrow keys once a chart has focus.
(function () {
    "use strict";

    // Pixels between the crosshair and the tooltip.
    var GAP = 10;

    function format(value, unit) {
        if (value === null || value === undefined) {
            return "-";
        }
        var text = value.toLocaleString(undefined, { maximumFractionDigits: 2 });
        return unit ? text + " " + unit : text;
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

        function show(index) {
            current = index;
            var left = data.x[index] * 100;
            crosshair.style.left = left + "%";
            crosshair.hidden = false;

            tooltip.replaceChildren(
                element("div", "tooltip-time", data.times[index] + " · " + data.every)
            );
            data.series.forEach(function (series) {
                if (series.mean[index] !== null) {
                    var row = element("div", "tooltip-row");
                    row.append(
                        element("span", "line-key s" + series.slot),
                        element("strong", "", format(series.mean[index], data.unit)),
                        element("span", "muted", series.name)
                    );
                    var low = series.low[index];
                    var high = series.high[index];
                    if (low !== null && high !== null && low !== high) {
                        row.append(element("span", "muted", format(low) + " to " + format(high)));
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
        plot.addEventListener("focus", function () {
            show(current);
        });
        plot.addEventListener("blur", hide);
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
            } else if (event.key === "Escape") {
                hide();
            }
        });
    }

    document.querySelectorAll("figure.chart[data-points]").forEach(setUp);
})();
