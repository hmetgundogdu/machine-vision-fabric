using Spectre.Console;
using Spectre.Console.Rendering;

namespace Mvf.Cli.Tui;

/// <summary>
/// Renders the pipeline graph as a Spectre.Console <see cref="IRenderable"/>.
///
/// Layout:
///   [Layer 0] ──edge──► [Layer 1] ──edge──► ...
///
/// Each layer column stacks node boxes vertically.
/// Edge columns show port-label arrows between layers.
/// Colour-codes by node status and category.
/// </summary>
public static class GraphRenderer
{
    // ── Status icons ────────────────────────────────────────────────────────
    private const string IconIdle    = "[grey]o[/]";
    private const string IconActive  = "[yellow]*[/]";
    private const string IconDone    = "[green]+[/]";
    private const string IconFaulted = "[red]![/]";

    // ── Category colours ────────────────────────────────────────────────────
    private static string CategoryColor(string category) => category.ToLowerInvariant() switch
    {
        "source"       => "dodgerblue1",
        "control"      => "orchid",
        "flow-control" => "gold1",
        "output" or "sink" => "mediumspringgreen",
        "compute"      => "steelblue1",
        _              => "grey84"
    };

    private static string EdgeColor(string kind) => kind.ToLowerInvariant() switch
    {
        "control" => "orchid",
        _         => "steelblue1"
    };

    public static IRenderable Render(
        GraphLayout layout,
        IReadOnlyDictionary<string, PipelineNodeState> nodeStates,
        int terminalWidth,
        int activeLayer)
    {
        var (viewStart, viewEnd) = layout.ComputeViewport(terminalWidth, activeLayer);

        // Determine max slots in viewport for height
        var maxSlots = Enumerable.Range(viewStart, viewEnd - viewStart)
            .Select(l => layout.Layers[l].Count)
            .DefaultIfEmpty(1)
            .Max();

        var totalRows = maxSlots * (GraphLayout.NodeBoxHeight + GraphLayout.NodeRowPad);

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
                    ? RenderNodeBox(layerNodes[slot], nodeStates)
                    : new Markup(string.Empty);

                rowCells.Add(nodeRenderable);

                // Edge column
                if (layerIdx < viewEnd - 1)
                {
                    var edgesHere = layout.Edges
                        .Where(e => e.From.Layer == layerIdx && e.From.Slot == slot)
                        .ToList();
                    rowCells.Add(RenderEdgeColumn(edgesHere, nodeStates));
                }
            }

            grid.AddRow([.. rowCells]);
        }

        // Viewport hint
        IRenderable hint = layout.LayerCount > (viewEnd - viewStart)
            ? new Markup($"[grey]◄ layers {viewStart + 1}–{viewEnd} of {layout.LayerCount} ►[/]\n")
            : new Markup(string.Empty);

        return new Rows(hint, grid);
    }

    private static IRenderable RenderNodeBox(
        string nodeId,
        IReadOnlyDictionary<string, PipelineNodeState> states)
    {
        if (!states.TryGetValue(nodeId, out var state))
            return new Markup($"[grey]{nodeId}[/]");

        var icon = state.Status switch
        {
            NodeLifecycleStatus.Active  => IconActive,
            NodeLifecycleStatus.Done    => IconDone,
            NodeLifecycleStatus.Faulted => IconFaulted,
            _                           => IconIdle
        };

        var catColor = CategoryColor(state.Category);
        var w = GraphLayout.NodeBoxWidth - 2; // inside border

        var title   = Truncate(state.DisplayName, w);
        var sub     = Truncate(state.TypeLabel, w);
        var cycles  = state.TotalCycles == 0
            ? "[grey42]waiting[/]"
            : $"[grey42]cyc[/][white]{state.TotalCycles}[/] [grey42]ok[/][green]{state.AcceptedCycles}[/]";
        var timing  = state.LastDurationMs > 0
            ? $"[grey42]{state.LastDurationMs}ms[/]"
            : string.Empty;

        var content = new Markup(
            $"{icon} [{catColor}]{Markup.Escape(title)}[/]\n" +
            $"[grey42]{Markup.Escape(sub)}[/]\n" +
            $"{cycles}  {timing}");

        var borderStyle = state.Status switch
        {
            NodeLifecycleStatus.Active  => Style.Parse("yellow"),
            NodeLifecycleStatus.Faulted => Style.Parse("red"),
            NodeLifecycleStatus.Done    => Style.Parse($"{catColor}"),
            _                           => Style.Parse("grey")
        };

        return new Panel(content)
        {
            Border = BoxBorder.Rounded,
            BorderStyle = borderStyle,
            Padding = new Padding(0, 0)
        };
    }

    private static IRenderable RenderEdgeColumn(
        List<GraphLayout.LayoutEdge> edges,
        IReadOnlyDictionary<string, PipelineNodeState> states)
    {
        if (edges.Count == 0)
            return new Markup("[grey]   [/]");

        var lines = new List<string>();
        foreach (var edge in edges)
        {
            var color = EdgeColor(edge.Kind);
            var label = Truncate(edge.FromPort, GraphLayout.EdgeColumnWidth - 6);
            lines.Add($"[{color}]--{Markup.Escape(label)}-->[/]");
        }

        return new Markup(string.Join("\n", lines));
    }

    private static string Truncate(string s, int max) =>
        s.Length <= max ? s : s[..(max - 1)] + "…";
}
