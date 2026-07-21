using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Execution;

/// <summary>
/// Produces a deterministic topological execution order for a pipeline graph.
/// Source and gate nodes (no data inputs) run first;
/// embedded primitives and sinks run after their upstream producers.
/// </summary>
internal static class GraphTopologySorter
{
    /// <summary>
    /// Returns nodes in topological order (Kahn's algorithm).
    /// Throws <see cref="InvalidOperationException"/> if the graph contains a cycle.
    /// </summary>
    public static IReadOnlyList<PipelineNodeDefinition> Sort(PipelineDefinition definition)
    {
        var nodeById = definition.Nodes.ToDictionary(
            n => n.Id,
            n => n,
            StringComparer.OrdinalIgnoreCase);

        // Build in-degree map based on data edges
        // (control edges also participate in ordering)
        var inDegree = definition.Nodes.ToDictionary(
            n => n.Id,
            _ => 0,
            StringComparer.OrdinalIgnoreCase);

        var adjacency = new Dictionary<string, List<string>>(StringComparer.OrdinalIgnoreCase);
        foreach (var node in definition.Nodes)
        {
            adjacency[node.Id] = [];
        }

        foreach (var edge in definition.Edges)
        {
            if (!nodeById.ContainsKey(edge.From.NodeId) || !nodeById.ContainsKey(edge.To.NodeId))
            {
                continue;
            }

            adjacency[edge.From.NodeId].Add(edge.To.NodeId);
            inDegree[edge.To.NodeId]++;
        }

        // Kahn's algorithm
        var queue = new Queue<string>(
            inDegree
                .Where(kv => kv.Value == 0)
                .Select(kv => kv.Key)
                .OrderBy(id => id, StringComparer.OrdinalIgnoreCase));

        var sorted = new List<PipelineNodeDefinition>(definition.Nodes.Count);

        while (queue.Count > 0)
        {
            var currentId = queue.Dequeue();
            if (!nodeById.TryGetValue(currentId, out var currentNode))
            {
                continue;
            }

            sorted.Add(currentNode);

            foreach (var neighbour in adjacency[currentId].OrderBy(id => id, StringComparer.OrdinalIgnoreCase))
            {
                inDegree[neighbour]--;
                if (inDegree[neighbour] == 0)
                {
                    queue.Enqueue(neighbour);
                }
            }
        }

        if (sorted.Count != definition.Nodes.Count)
        {
            throw new InvalidOperationException(
                $"Pipeline graph '{definition.Name}' contains a cycle. " +
                $"Topological sort yielded {sorted.Count} of {definition.Nodes.Count} nodes.");
        }

        return sorted;
    }
}
