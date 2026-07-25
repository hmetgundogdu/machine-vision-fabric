using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Graph.Execution;
using Mvf.Graph.Values;

namespace Mvf.Engine.Execution.NodeRunners;

/// <summary>
/// Embedded-primitive <c>select</c> node runner: narrows a collection with a criterion.
///
/// <para>The criterion has three sources and they are all the same thing by the time it gets here —
/// written in config (<c>where</c>), resolved before the run by the pre-pass (from the binding store or
/// an operator), or arriving on the <c>criterion</c> port from a <c>value</c> node. An edge wins over
/// the static one, so a criterion that is genuinely per-frame stays per-frame.</para>
///
/// <para>This is <b>not</b> <c>switch</c>. <c>switch</c> routes one frame to one of several output ports
/// by a control signal; <c>select</c> turns a collection into a smaller collection or a single element.
/// Different inputs, different outputs, no overlap.</para>
///
/// Input ports : <c>items</c> (control — <c>control/list:&lt;t&gt;</c>),
///               <c>criterion</c> (control — <c>control/value:&lt;t&gt;</c>, optional)
/// Output ports: <c>selected</c> — <c>control/value:&lt;t&gt;</c> in mode <c>one</c>,
///               <c>control/list:&lt;t&gt;</c> in mode <c>many</c>
/// </summary>
internal sealed class SelectPrimitiveNodeRunner(
    string nodeId,
    SelectMode mode,
    JsonNode? staticCriterion,
    string? by,
    LiveValue? live = null) : INodeRunner
{
    private JsonArray? _publishedItems;

    public string NodeId { get; } = nodeId;

    public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
    {
        if (inputs.Get("items")?.Control?.Payload is not JsonArray items)
        {
            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        // Publish what is actually being narrowed, so a picker offered mid-run shows the current
        // candidates rather than the ones the run started with. Reference-compared, so a steady collection
        // costs one comparison per cycle and never clones.
        if (live is not null && !ReferenceEquals(items, _publishedItems))
        {
            _publishedItems = items;
            live.PublishChoices(items.Select(item => item?.DeepClone()).ToList());
        }

        // Precedence: an edge is a genuinely per-frame criterion and outranks everything; then whatever an
        // operator tuned mid-run; then what the pre-pass or config settled.
        var criterion = inputs.Get("criterion")?.Control is { } edgeCriterion
            ? edgeCriterion.Payload
            : live?.Current ?? staticCriterion;

        if (criterion is null)
        {
            return Task.FromResult(NodeExecutionResult.NoOutput);
        }

        if (mode == SelectMode.Many)
        {
            var narrowed = new JsonArray();
            foreach (var item in items)
            {
                if (Matches(item, criterion, by))
                {
                    narrowed.Add(item?.DeepClone());
                }
            }

            return Task.FromResult(NodeExecutionResult.Single(
                "selected", PortValue.FromControl(ControlSignal.FromList(narrowed, NodeId))));
        }

        foreach (var item in items)
        {
            if (!Matches(item, criterion, by))
            {
                continue;
            }

            return Task.FromResult(NodeExecutionResult.Single(
                "selected", PortValue.FromControl(ControlSignal.FromValue(item?.DeepClone(), NodeId))));
        }

        // Nothing matched. Emitting nothing is the honest answer — a downstream node then simply does not
        // run this cycle, exactly as for a closed gate.
        return Task.FromResult(NodeExecutionResult.NoOutput);
    }

    /// <summary>
    /// Three comparison shapes, in the order they disambiguate:
    /// <list type="bullet">
    ///   <item><c>by</c> names a property — compare <c>item[by]</c> with the criterion.</item>
    ///   <item>no <c>by</c>, object criterion — every property in the criterion must match the item's.</item>
    ///   <item>otherwise — compare the element itself.</item>
    /// </list>
    /// </summary>
    private static bool Matches(JsonNode? item, JsonNode criterion, string? by)
    {
        if (by is { Length: > 0 })
        {
            return item is JsonObject obj
                && obj.TryGetPropertyValue(by, out var property)
                && JsonNode.DeepEquals(property, criterion);
        }

        if (criterion is JsonObject filter)
        {
            if (item is not JsonObject candidate)
            {
                return false;
            }

            foreach (var (name, expected) in filter)
            {
                if (!candidate.TryGetPropertyValue(name, out var actual) || !JsonNode.DeepEquals(actual, expected))
                {
                    return false;
                }
            }

            return true;
        }

        return JsonNode.DeepEquals(item, criterion);
    }

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}
