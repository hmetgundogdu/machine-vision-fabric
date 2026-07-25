using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Graph.Execution;
using Mvf.Graph.Values;

namespace Mvf.Engine.Execution.NodeRunners;

/// <summary>
/// Embedded-primitive <c>value</c> node runner: emits exactly one typed value the graph could not
/// compute.
///
/// <para>The value is already resolved when this runs — the binding pre-pass settled it before cycle 0,
/// from a literal, the binding store, a resolver, or a default. Nothing here asks anyone anything: the
/// cycle loop runs per frame, and prompting a human inside it would break unattended operation.</para>
///
/// <para><b>It may still change.</b> The setting lives in a <see cref="LiveValue"/> that an operator can
/// turn mid-run, and this reads it once per cycle. That does not reintroduce the problem prompting had —
/// the loop never waits for anybody, it simply observes whatever the current setting is on its next pass.
/// The emitted result is cached and only rebuilt when the setting actually changed, so an untouched value
/// costs one volatile read per cycle.</para>
///
/// Input ports : none
/// Output ports: <c>value</c> (control — <c>control/value:&lt;t&gt;</c>)
/// </summary>
internal sealed class ValuePrimitiveNodeRunner : INodeRunner
{
    private readonly LiveValue _live;
    private JsonNode? _emitted;
    private NodeExecutionResult _result;

    public ValuePrimitiveNodeRunner(string nodeId, LiveValue live)
    {
        NodeId = nodeId;
        _live = live;
        _emitted = live.Current;
        _result = Build(nodeId, _emitted);
    }

    public string NodeId { get; }

    public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        var current = _live.Current;

        // Reference comparison is enough: a change always installs a freshly cloned node, so a differing
        // reference is exactly "somebody set this".
        if (!ReferenceEquals(current, _emitted))
        {
            _emitted = current;
            _result = Build(NodeId, current);
        }

        return Task.FromResult(_result);
    }

    /// <summary>
    /// A list-shaped value goes out as a <c>list</c> signal so a <c>select</c> can consume it on its items
    /// port; everything else is a single <c>value</c> signal.
    /// </summary>
    private NodeExecutionResult Build(string nodeId, JsonNode? value)
    {
        var signal = _live.Shape == ControlValueShape.List && value is JsonArray items
            ? ControlSignal.FromList(items, nodeId)
            : ControlSignal.FromValue(value, nodeId);

        return NodeExecutionResult.Single("value", PortValue.FromControl(signal));
    }

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}
