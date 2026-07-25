using System.Text.Json.Nodes;

namespace Mvf.Graph.Values;

/// <summary>
/// One <c>value</c> node's current setting, readable by the running graph and writable from outside it.
///
/// <para>The read is a plain volatile read on the hot path — a node picks up a new setting on its
/// <b>next cycle</b> and never waits for anyone. That distinction is the whole reason live editing does
/// not break the design: prompting inside the loop binds the pipeline to a human, whereas an
/// asynchronous setting change does not. The loop stays exactly as unattended as it was.</para>
/// </summary>
public sealed class LiveValue
{
    private JsonNode? _current;
    private IReadOnlyList<JsonNode?>? _choices;

    internal LiveValue(
        string nodeId,
        string label,
        ControlValueType type,
        ControlValueShape shape,
        JsonNode? schema,
        string? binding,
        JsonNode? initial,
        string? choiceLabelProperty)
    {
        NodeId = nodeId;
        Label = label;
        Type = type;
        Shape = shape;
        Schema = schema;
        Binding = binding;
        _current = initial;
        ChoiceLabelProperty = choiceLabelProperty;
    }

    public string NodeId { get; }

    /// <summary>What to call it on screen — the node's <c>prompt</c>, else its display name.</summary>
    public string Label { get; }

    public ControlValueType Type { get; }

    public ControlValueShape Shape { get; }

    public JsonNode? Schema { get; }

    /// <summary>
    /// Where a change is persisted, or null. A node with only a <c>literal</c> has nowhere durable to
    /// write to: persisting it would mean editing <c>pipeline.json</c>, and that file is the portable,
    /// versioned artifact that deploys byte-identically to every machine. Such a value is still tunable —
    /// the change simply lasts for the run.
    /// </summary>
    public string? Binding { get; }

    public JsonNode? Current => Volatile.Read(ref _current);

    /// <summary>
    /// The collection this setting is chosen from, when there is one — a <c>select</c>'s items, published
    /// by the node itself as it sees them. Null means "free-form": type it.
    ///
    /// <para>Published from the running graph rather than resolved once up front, so a picker offered
    /// mid-run shows what the pipeline is <i>actually</i> narrowing right now, not what it was narrowing
    /// when the process started.</para>
    /// </summary>
    public IReadOnlyList<JsonNode?>? Choices => Volatile.Read(ref _choices);

    /// <summary>
    /// The property of a chosen element that is stored, from <c>select</c>'s <c>by</c>. Storing the
    /// identifier rather than the whole record is what lets a binding survive a later discovery run
    /// returning the same device with different incidental fields.
    /// </summary>
    public string? ChoiceLabelProperty { get; }

    internal void Set(JsonNode? value) => Volatile.Write(ref _current, value);

    /// <summary>
    /// Called by the node itself with the collection it is narrowing. Public where <see cref="Set"/> is
    /// not, and the asymmetry is the contract: a new <b>setting</b> must pass the type and schema check in
    /// <see cref="LiveValueRegistry.TrySet"/>, whereas candidates are an observation of what the graph is
    /// already carrying — nothing to validate, and nothing that changes what the node emits.
    /// </summary>
    public void PublishChoices(IReadOnlyList<JsonNode?>? choices) => Volatile.Write(ref _choices, choices);
}

/// <summary>
/// The live tunables of a run: every <c>value</c> node, and the one place anything outside the executor
/// may change one.
///
/// <para>What can be tuned live is decided by <b>when the value is consumed</b>. A threshold is consumed
/// per frame, so turning it mid-run is meaningful. Which camera to open is consumed at activation, so
/// "changing" it means closing and reopening a device — that is reconfiguration, not tuning, and it is
/// deliberately not offered here.</para>
/// </summary>
public sealed class LiveValueRegistry
{
    private readonly List<LiveValue> _values = [];
    private readonly Lock _gate = new();

    /// <summary>Raised after a successful change, so a host can persist it. Never raised on the hot path.</summary>
    public event Action<LiveValue>? Changed;

    /// <summary>
    /// The run's running/paused state, read by the executor once per iteration and toggled from a CLI
    /// shortcut. Always present; the executor only gates on it when the graph carries a <c>loop</c>, and a
    /// host only surfaces the control then. It lives here — on the same registry as the live tunables —
    /// because it is the same kind of thing: a boolean the running graph observes and the operator turns,
    /// without ever blocking the loop.
    /// </summary>
    public RunControl RunControl { get; } = new();

    public IReadOnlyList<LiveValue> Values
    {
        get { lock (_gate) { return _values.ToArray(); } }
    }

    /// <summary>
    /// Registers a node's value. Re-registering the same node id replaces the entry, so a re-run inside
    /// one process does not accumulate stale nodes.
    /// </summary>
    public LiveValue Register(
        string nodeId,
        string label,
        ControlValueType type,
        JsonNode? schema,
        string? binding,
        JsonNode? initial,
        ControlValueShape shape = ControlValueShape.Single,
        string? choiceLabelProperty = null)
    {
        var live = new LiveValue(nodeId, label, type, shape, schema, binding, initial, choiceLabelProperty);

        lock (_gate)
        {
            _values.RemoveAll(v => string.Equals(v.NodeId, nodeId, StringComparison.OrdinalIgnoreCase));
            _values.Add(live);
        }

        return live;
    }

    public void Clear()
    {
        lock (_gate) { _values.Clear(); }
    }

    public LiveValue? Find(string nodeId)
    {
        lock (_gate)
        {
            return _values.FirstOrDefault(v => string.Equals(v.NodeId, nodeId, StringComparison.OrdinalIgnoreCase));
        }
    }

    /// <summary>
    /// Applies a new setting after checking it against the declared type and schema — the same check a
    /// literal, a stored binding or an operator's first answer goes through. A running graph is the last
    /// place that should be allowed to receive an ill-typed value.
    /// </summary>
    public bool TrySet(string nodeId, JsonNode? value, out string? error)
    {
        var live = Find(nodeId);
        if (live is null)
        {
            error = $"'{nodeId}' is not a tunable value in this run.";
            return false;
        }

        if (!ControlValueTypes.MatchesShape(live.Shape, live.Type, value))
        {
            error = live.Shape == ControlValueShape.List
                ? $"expected a list of {ControlValueTypes.ToToken(live.Type)}"
                : $"expected {ControlValueTypes.ToToken(live.Type)}";
            return false;
        }

        if (!JsonSchemaCheck.TryValidateShaped(live.Shape, live.Schema, value, out var schemaError))
        {
            error = schemaError;
            return false;
        }

        live.Set(value?.DeepClone());
        error = null;
        Changed?.Invoke(live);
        return true;
    }
}
