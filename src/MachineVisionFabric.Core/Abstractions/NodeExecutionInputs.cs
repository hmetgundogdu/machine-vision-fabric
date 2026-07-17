namespace MachineVisionFabric.Core.Abstractions;

/// <summary>
/// Immutable map of port name → value passed into a node during execution.
/// </summary>
public sealed class NodeExecutionInputs
{
    public static readonly NodeExecutionInputs Empty = new(new Dictionary<string, PortValue>(), null);

    private readonly IReadOnlyDictionary<string, PortValue> _values;

    public NodeExecutionInputs(
        IReadOnlyDictionary<string, PortValue> values,
        NodeExecutionContext? context = null)
    {
        ArgumentNullException.ThrowIfNull(values);
        _values = values;
        Context = context;
    }

    /// <summary>Per-cycle execution context. Null only when constructed without context (tests/stubs).</summary>
    public NodeExecutionContext? Context { get; }

    public PortValue? Get(string portName) =>
        _values.TryGetValue(portName, out var value) ? value : null;

    public bool Has(string portName) => _values.ContainsKey(portName);

    public IEnumerable<KeyValuePair<string, PortValue>> All => _values;
}
