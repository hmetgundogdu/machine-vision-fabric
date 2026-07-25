using Mvf.Graph.Pipelines;

namespace Mvf.Cli.Tui;

/// <summary>
/// Assigns each pipeline node to a (layer, slot) coordinate using Kahn's topological layering,
/// then computes a viewport window for rendering within a terminal width limit.
/// </summary>
public sealed class GraphLayout
{
    // Deliberately compact: the smaller a node box is, the more of the graph fits at once and the less
    // often the viewport has to scroll. Detail lives in the node detail view, not in the box.
    public const int NodeBoxWidth    = 22;
    public const int NodeBoxHeight   = 5;
    public const int EdgeColumnWidth = 13;
    public const int NodeRowPad      = 1;

    /// <summary>Node ID → (layer index, slot index within layer).</summary>
    public IReadOnlyDictionary<string, (int Layer, int Slot)> NodePositions { get; }

    /// <summary>Total number of layers.</summary>
    public int LayerCount { get; }

    /// <summary>Per-layer list of node IDs (in slot order).</summary>
    public IReadOnlyList<IReadOnlyList<string>> Layers { get; }

    /// <summary>Edges with computed source and target positions.</summary>
    public IReadOnlyList<LayoutEdge> Edges { get; }

    /// <summary>
    /// Every node id in a single left-to-right, top-to-bottom order (layer then slot). This is the order
    /// the operator's ←/→ keys walk, so stepping right always lands on the visually next node.
    /// </summary>
    public IReadOnlyList<string> TraversalOrder { get; }

    private GraphLayout(
        IReadOnlyDictionary<string, (int, int)> positions,
        IReadOnlyList<IReadOnlyList<string>> layers,
        IReadOnlyList<LayoutEdge> edges)
    {
        NodePositions = positions;
        LayerCount = layers.Count;
        Layers = layers;
        Edges = edges;
        TraversalOrder = layers.SelectMany(l => l).ToList();
    }

    /// <summary>The layer a node sits in, or 0 when it is unknown to the layout.</summary>
    public int LayerOf(string nodeId) =>
        NodePositions.TryGetValue(nodeId, out var pos) ? pos.Layer : 0;

    public static GraphLayout Build(PipelineDefinition definition)
    {
        var nodes = definition.Nodes;
        var edges = definition.Edges;

        // --- Kahn's layering: layer = longest-path distance from sources ---
        var inEdge = nodes.ToDictionary(n => n.Id, _ => new List<string>(), StringComparer.OrdinalIgnoreCase);
        var outEdge = nodes.ToDictionary(n => n.Id, _ => new List<string>(), StringComparer.OrdinalIgnoreCase);
        foreach (var e in edges)
        {
            if (inEdge.ContainsKey(e.To.NodeId) && outEdge.ContainsKey(e.From.NodeId))
            {
                inEdge[e.To.NodeId].Add(e.From.NodeId);
                outEdge[e.From.NodeId].Add(e.To.NodeId);
            }
        }

        var layer = nodes.ToDictionary(n => n.Id, _ => 0, StringComparer.OrdinalIgnoreCase);
        // Propagate: layer[v] = max(layer[u]+1) for all in-edges u→v
        var topOrder = TopologicalOrder(nodes.Select(n => n.Id).ToList(), inEdge);
        foreach (var nodeId in topOrder)
        {
            foreach (var successor in outEdge[nodeId])
            {
                layer[successor] = Math.Max(layer[successor], layer[nodeId] + 1);
            }
        }

        // Group nodes by layer
        var maxLayer = layer.Values.DefaultIfEmpty(0).Max();
        var layerLists = Enumerable.Range(0, maxLayer + 1)
            .Select(l => (IReadOnlyList<string>)nodes
                .Where(n => layer[n.Id] == l)
                .Select(n => n.Id)
                .ToList())
            .ToList();

        // Build positions dict
        var positions = new Dictionary<string, (int, int)>(StringComparer.OrdinalIgnoreCase);
        for (var l = 0; l < layerLists.Count; l++)
        {
            for (var s = 0; s < layerLists[l].Count; s++)
            {
                positions[layerLists[l][s]] = (l, s);
            }
        }

        // Build layout edges
        var layoutEdges = edges
            .Where(e => positions.ContainsKey(e.From.NodeId) && positions.ContainsKey(e.To.NodeId))
            .Select(e => new LayoutEdge(
                e.Id,
                e.From.NodeId,
                e.To.NodeId,
                e.From.Port,
                e.To.Port,
                e.Kind,
                positions[e.From.NodeId],
                positions[e.To.NodeId]))
            .ToList();

        return new GraphLayout(positions, layerLists, layoutEdges);
    }

    private static List<string> TopologicalOrder(
        List<string> nodeIds,
        Dictionary<string, List<string>> inEdge)
    {
        var inDegree = nodeIds.ToDictionary(id => id, id => inEdge[id].Count, StringComparer.OrdinalIgnoreCase);
        var queue = new Queue<string>(nodeIds.Where(id => inDegree[id] == 0).OrderBy(id => id));
        var order = new List<string>(nodeIds.Count);
        while (queue.Count > 0)
        {
            var current = queue.Dequeue();
            order.Add(current);
            // We need out-edges here — reconstruct from inEdge
            foreach (var other in nodeIds)
            {
                if (inEdge[other].Contains(current, StringComparer.OrdinalIgnoreCase))
                {
                    inDegree[other]--;
                    if (inDegree[other] == 0) queue.Enqueue(other);
                }
            }
        }
        return order;
    }

    /// <summary>
    /// Computes the range of layers [start, end) to show given the terminal width
    /// and the "active" layer index (current execution focus).
    /// </summary>
    public (int Start, int End) ComputeViewport(int terminalWidth, int activeLayer)
    {
        // Width of rendering one layer + one edge column
        var unitWidth = NodeBoxWidth + EdgeColumnWidth;
        var visibleLayers = Math.Max(1, (terminalWidth - 4) / unitWidth);

        if (visibleLayers >= LayerCount)
            return (0, LayerCount);

        // Slide window to keep active layer roughly centred
        var half = visibleLayers / 2;
        var start = Math.Max(0, activeLayer - half);
        var end = Math.Min(LayerCount, start + visibleLayers);
        start = Math.Max(0, end - visibleLayers);
        return (start, end);
    }

    public readonly record struct LayoutEdge(
        string EdgeId,
        string FromNode,
        string ToNode,
        string FromPort,
        string ToPort,
        string Kind,
        (int Layer, int Slot) From,
        (int Layer, int Slot) To);
}
