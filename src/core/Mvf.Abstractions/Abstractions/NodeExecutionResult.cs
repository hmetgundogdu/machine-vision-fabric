namespace Mvf.Abstractions;

/// <summary>
/// Immutable map of port name → value produced by a node after execution.
/// An empty result signals that the node produced no output this cycle
/// (e.g. stream exhausted, gate closed, primitive blocked).
/// </summary>
public sealed class NodeExecutionResult
{
    public static readonly NodeExecutionResult NoOutput = new(new Dictionary<string, PortValue>());

    private readonly IReadOnlyDictionary<string, PortValue> _values;

    public NodeExecutionResult(IReadOnlyDictionary<string, PortValue> values)
    {
        ArgumentNullException.ThrowIfNull(values);
        _values = values;
    }

    public bool HasOutput => _values.Count > 0;

    public PortValue? Get(string portName) =>
        _values.TryGetValue(portName, out var value) ? value : null;

    public IEnumerable<KeyValuePair<string, PortValue>> All => _values;

    public static NodeExecutionResult Single(string portName, PortValue value)
    {
        ArgumentNullException.ThrowIfNull(portName);
        ArgumentNullException.ThrowIfNull(value);
        return new NodeExecutionResult(new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase)
        {
            [portName] = value
        });
    }

    public static NodeExecutionResult FromPairs(params (string portName, PortValue value)[] pairs)
    {
        var dict = new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase);
        foreach (var (portName, value) in pairs)
        {
            dict[portName] = value;
        }

        return new NodeExecutionResult(dict);
    }
}
