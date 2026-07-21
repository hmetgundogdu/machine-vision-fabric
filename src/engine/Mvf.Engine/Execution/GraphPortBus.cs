using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Abstractions;

namespace Mvf.Engine.Execution;

/// <summary>
/// In-memory port value bus used during a single execution cycle.
/// Maps (nodeId, portName) → PortValue for routing between nodes.
/// </summary>
internal sealed class GraphPortBus
{
    private readonly Dictionary<(string NodeId, string Port), PortValue> _values =
        new(EqualityComparer<(string, string)>.Create(
            (a, b) => StringComparer.OrdinalIgnoreCase.Equals(a.Item1, b.Item1)
                   && StringComparer.OrdinalIgnoreCase.Equals(a.Item2, b.Item2),
            obj => HashCode.Combine(
                StringComparer.OrdinalIgnoreCase.GetHashCode(obj.Item1),
                StringComparer.OrdinalIgnoreCase.GetHashCode(obj.Item2))));

    public void Set(string nodeId, string portName, PortValue value) =>
        _values[(nodeId, portName)] = value;

    public PortValue? Get(string nodeId, string portName) =>
        _values.TryGetValue((nodeId, portName), out var v) ? v : null;

    /// <summary>
    /// Routes all output values produced by a node to the downstream input ports
    /// of connected nodes according to the edge definitions.
    /// </summary>
    public void RouteOutputs(
        string sourceNodeId,
        NodeExecutionResult result,
        IReadOnlyList<PipelineEdgeDefinition> edges)
    {
        foreach (var (portName, portValue) in result.All)
        {
            foreach (var edge in edges)
            {
                if (!StringComparer.OrdinalIgnoreCase.Equals(edge.From.NodeId, sourceNodeId)
                    || !StringComparer.OrdinalIgnoreCase.Equals(edge.From.Port, portName))
                {
                    continue;
                }

                Set(edge.To.NodeId, edge.To.Port, portValue);
            }
        }
    }

    /// <summary>
    /// Collects all input values for a node from currently routed port values.
    /// </summary>
    public NodeExecutionInputs CollectInputs(
        string nodeId,
        IReadOnlyList<PipelinePortDefinition> inputPorts,
        NodeExecutionContext? context = null)
    {
        if (inputPorts.Count == 0)
        {
            return context is null
                ? NodeExecutionInputs.Empty
                : new NodeExecutionInputs(new Dictionary<string, PortValue>(), context);
        }

        var values = new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase);
        foreach (var port in inputPorts)
        {
            var value = Get(nodeId, port.Name);
            if (value is not null)
            {
                values[port.Name] = value;
            }
        }

        return new NodeExecutionInputs(values, context);
    }

    public void ClearCycle() => _values.Clear();
}
