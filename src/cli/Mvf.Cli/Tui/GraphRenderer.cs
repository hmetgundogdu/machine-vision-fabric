using Spectre.Console;
using Spectre.Console.Rendering;

namespace Mvf.Cli.Tui;

/// <summary>
/// Renders the pipeline graph as a Spectre.Console <see cref="IRenderable"/>.
///
/// Layout:
///   [Layer 0] ──edge──► [Layer 1] ──edge──► ...
///
/// Each layer column stacks node boxes vertically; edge columns show typed port arrows between them.
/// A box is colour-coded by category and kind; the node currently executing pulses, and the operator's
/// navigation cursor is drawn with a heavy cyan border. A one-line config summary sits inside each box so
/// the live settings read next to the node they belong to.
/// </summary>
public static class GraphRenderer
{
    /// <summary>A node's in-box config line: pre-formatted text plus whether it is live-editable.</summary>
    public readonly record struct NodeConfigLine(string Text, bool Tunable);

    // ── Status icons ────────────────────────────────────────────────────────
    private const string IconIdle    = "[grey42]○[/]";
    private const string IconActive  = "[yellow1]▶[/]";
    private const string IconDone    = "[green3]✔[/]";
    private const string IconFaulted = "[red1]✘[/]";

    // ── Category colours (the node title) ────────────────────────────────────
    // Categories are the ones PipelineExpander assigns: source / compute / classify / control /
    // output / value / flow-control. Each family gets a distinct hue so the graph reads at a glance.
    private static string CategoryColor(string category) => category.ToLowerInvariant() switch
    {
        "source"            => "deepskyblue1",
        "compute"           => "mediumpurple2",
        "classify"          => "hotpink",
        "control"           => "orchid",
        "flow-control"      => "gold1",
        "output" or "sink"  => "mediumspringgreen",
        "value"             => "aquamarine1",
        _                   => "grey84"
    };

    // ── Kind badge (a single tinted letter that says how the node is hosted) ──
    private static (string Glyph, string Color) KindBadge(string kind) => kind.ToLowerInvariant() switch
    {
        "integration-module" => ("M", "steelblue1"),
        "embedded-primitive" => ("P", "gold1"),
        "runtime-builtin"    => ("B", "mediumspringgreen"),
        _                    => ("·", "grey54")
    };

    private static string EdgeColor(string kind) => kind.ToLowerInvariant() switch
    {
        "control" => "orchid",
        _         => "steelblue1"
    };

    public static IRenderable Render(
        GraphLayout layout,
        IReadOnlyDictionary<string, PipelineNodeState> nodeStates,
        IReadOnlyDictionary<string, NodeConfigLine> configLines,
        string? selectedNodeId,
        string? activeNodeId,
        int terminalWidth,
        int anchorLayer)
    {
        var (viewStart, viewEnd) = layout.ComputeViewport(terminalWidth, anchorLayer);

        // Determine max slots in viewport for height
        var maxSlots = Enumerable.Range(viewStart, viewEnd - viewStart)
            .Select(l => layout.Layers[l].Count)
            .DefaultIfEmpty(1)
            .Max();

        // Build a list of column renderables (tables within a grid)
        var grid = new Grid();

        for (var layerIdx = viewStart; layerIdx < viewEnd; layerIdx++)
        {
            // Node column
            grid.AddColumn(new GridColumn().NoWrap().Width(GraphLayout.NodeBoxWidth));

            // Edge column (unless last layer in viewport)
            if (layerIdx < viewEnd - 1)
            {
                grid.AddColumn(new GridColumn().NoWrap().Width(GraphLayout.EdgeColumnWidth));
            }
        }

        // Build rows: one row per "slot" across all layers in viewport
        for (var slot = 0; slot < maxSlots; slot++)
        {
            var rowCells = new List<IRenderable>();

            for (var layerIdx = viewStart; layerIdx < viewEnd; layerIdx++)
            {
                var layerNodes = layout.Layers[layerIdx];
                var nodeRenderable = slot < layerNodes.Count
                    ? RenderNodeBox(layerNodes[slot], nodeStates, configLines, selectedNodeId, activeNodeId)
                    : new Markup(string.Empty);

                rowCells.Add(nodeRenderable);

                // Edge column
                if (layerIdx < viewEnd - 1)
                {
                    var edgesHere = layout.Edges
                        .Where(e => e.From.Layer == layerIdx && e.From.Slot == slot)
                        .ToList();
                    rowCells.Add(RenderEdgeColumn(edgesHere));
                }
            }

            grid.AddRow([.. rowCells]);
        }

        // Viewport hint — only when the graph is wider than the window.
        IRenderable hint = layout.LayerCount > (viewEnd - viewStart)
            ? new Markup($"[grey50]◄ layers {viewStart + 1}–{viewEnd} of {layout.LayerCount} ►[/]\n")
            : new Markup(string.Empty);

        return new Rows(hint, grid);
    }

    private static IRenderable RenderNodeBox(
        string nodeId,
        IReadOnlyDictionary<string, PipelineNodeState> states,
        IReadOnlyDictionary<string, NodeConfigLine> configLines,
        string? selectedNodeId,
        string? activeNodeId)
    {
        if (!states.TryGetValue(nodeId, out var state))
            return new Markup($"[grey]{Markup.Escape(nodeId)}[/]");

        var isSelected = string.Equals(nodeId, selectedNodeId, StringComparison.OrdinalIgnoreCase);
        var isActive   = string.Equals(nodeId, activeNodeId, StringComparison.OrdinalIgnoreCase)
                         && state.Status != NodeLifecycleStatus.Faulted;

        var icon = isActive
            ? IconActive
            : state.Status switch
            {
                NodeLifecycleStatus.Done    => IconDone,
                NodeLifecycleStatus.Faulted => IconFaulted,
                _                           => IconIdle
            };

        var catColor = CategoryColor(state.Category);
        var w = GraphLayout.NodeBoxWidth - 2; // inside border

        // Title — inverse bar when the cursor is here so the eye lands on it instantly. The selected form
        // spends two extra columns on the padding inside the highlight, so its name is truncated shorter.
        var title = isSelected
            ? $"{icon} [black on deepskyblue1] {Markup.Escape(Truncate(state.DisplayName, w - 4))} [/]"
            : $"{icon} [{catColor}]{Markup.Escape(Truncate(state.DisplayName, w - 2))}[/]";

        var (badge, badgeColor) = KindBadge(state.Kind);
        var typeLine = $"[{badgeColor}]{badge}[/] [grey54]{Markup.Escape(Truncate(state.TypeLabel, w - 2))}[/]";

        var lines = new List<string> { title, typeLine };

        // Config summary lives inside the box, right under the node it configures.
        if (configLines.TryGetValue(nodeId, out var cfg) && cfg.Text.Length > 0)
        {
            var color = cfg.Tunable ? "deepskyblue1" : "grey66";
            var tail  = cfg.Tunable ? " [grey42]✎[/]" : string.Empty;
            lines.Add($"[{color}]{Markup.Escape(Truncate(cfg.Text, w - (cfg.Tunable ? 2 : 0)))}[/]{tail}");
        }

        var cycles = state.TotalCycles == 0
            ? "[grey42]waiting[/]"
            : $"[grey42]×[/][white]{state.TotalCycles}[/] [grey42]ok[/][green3]{state.AcceptedCycles}[/]";
        var timing = state.LastDurationMicros > 0
            ? $" [grey46]{DurationText.Format(state.LastDurationMicros)}[/]"
            : string.Empty;
        var restarts = state.WorkerRestarts > 0
            ? $" [red1]r{state.WorkerRestarts}[/]"
            : string.Empty;
        var faults = state.FaultedCycles > 0
            ? $" [red1]!{state.FaultedCycles}[/]"
            : string.Empty;
        lines.Add($"{cycles}{timing}{restarts}{faults}");

        var content = new Markup(string.Join("\n", lines));

        var (border, borderStyle) =
            isSelected                                 ? (BoxBorder.Heavy,   Style.Parse("deepskyblue1 bold")) :
            isActive                                   ? (BoxBorder.Rounded, Style.Parse("yellow1 bold")) :
            state.Status == NodeLifecycleStatus.Faulted ? (BoxBorder.Rounded, Style.Parse("red1")) :
            state.Status == NodeLifecycleStatus.Done    ? (BoxBorder.Rounded, Style.Parse(catColor)) :
                                                          (BoxBorder.Rounded, Style.Parse("grey35"));

        return new Panel(content)
        {
            Border = border,
            BorderStyle = borderStyle,
            Padding = new Padding(0, 0)
        };
    }

    private static IRenderable RenderEdgeColumn(List<GraphLayout.LayoutEdge> edges)
    {
        if (edges.Count == 0)
            return new Markup("[grey]   [/]");

        var lines = new List<string>();
        foreach (var edge in edges)
        {
            var color = EdgeColor(edge.Kind);
            // Control edges are dashed so a data/control mix is legible at a glance, not just by colour.
            var shaft = edge.Kind.Equals("control", StringComparison.OrdinalIgnoreCase) ? "╌" : "─";
            var label = Truncate(edge.FromPort, GraphLayout.EdgeColumnWidth - 5);
            lines.Add($"[{color}]{shaft}{Markup.Escape(label)}▶[/]");
        }

        return new Markup(string.Join("\n", lines));
    }

    private static string Truncate(string s, int max)
    {
        if (max <= 0) return string.Empty;
        return s.Length <= max ? s : s[..(max - 1)] + "…";
    }
}
