using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Engine.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Graph.Values;

namespace Mvf.Engine.Values;

/// <summary>
/// Resolves every <c>value</c> and <c>select</c> node <b>before execution starts</b>, so the engine only
/// ever sees constants.
///
/// <para>This is deliberately not inside the cycle loop. The graph is a per-frame loop: asking an
/// operator once per frame is absurd, and blocking the loop on a human breaks unattended operation. It
/// also keeps the terminal free — the TUI repaints the whole screen every 120 ms, so a prompt cannot
/// share it, and the pre-pass finishes before the dashboard starts.</para>
///
/// <para>The edges are still real. They carry the declared types, the validator checks them, and the
/// studio can draw <c>discover → select → camera</c>. They are simply not what moves the value at run
/// time.</para>
/// </summary>
public sealed class BindingPrePass(IValueBindingStore store, IValueResolver? resolver = null)
{
    public async Task<BindingPrePassResult> RunAsync(
        PipelineDefinition definition,
        CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(definition);

        var errors = new List<string>();
        var resolved = 0;
        var prompted = 0;

        var stored = await store.LoadAsync(cancellationToken);
        var bindings = new Dictionary<string, JsonNode?>(stored, StringComparer.Ordinal);
        var bindingsChanged = false;

        // Topological order, so a node that supplies another's candidates is resolved first — a picker
        // cannot offer a list that has not been settled yet. A cyclic graph falls back to file order; the
        // validator reports the cycle, and failing here would only hide it behind a worse message.
        IReadOnlyList<PipelineNodeDefinition> order;
        try
        {
            order = GraphTopologySorter.Sort(definition);
        }
        catch (InvalidOperationException)
        {
            order = definition.Nodes;
        }

        // Which criterion ports already have a producer: those are per-frame criteria and are none of the
        // pre-pass's business.
        var wiredPorts = definition.Edges
            .Select(edge => (edge.To.NodeId, edge.To.Port))
            .ToHashSet(EqualityComparer<(string NodeId, string Port)>.Create(
                (a, b) => StringComparer.OrdinalIgnoreCase.Equals(a.NodeId, b.NodeId)
                       && StringComparer.OrdinalIgnoreCase.Equals(a.Port, b.Port),
                key => HashCode.Combine(
                    StringComparer.OrdinalIgnoreCase.GetHashCode(key.NodeId),
                    StringComparer.OrdinalIgnoreCase.GetHashCode(key.Port))));

        foreach (var node in order)
        {
            // Live-editable module config: overlay any stored edit onto the node's config before it
            // activates, so yesterday's CLI change comes back today. Only reads the store here — writes
            // happen when the operator edits a tunable mid-run. Applies to any node kind that declares
            // bindings; the embedded-primitive handling below is unaffected.
            if (node.Bindings.Count > 0)
            {
                foreach (var binding in ModuleBindings.Read(node.Bindings))
                {
                    if (binding.Binding is not { Length: > 0 } key || !bindings.TryGetValue(key, out var storedField))
                    {
                        continue;
                    }

                    if (!AcceptsModuleBinding(binding, storedField, out var bindingError))
                    {
                        errors.Add(
                            $"Node '{node.Id}': stored binding '{key}' for field '{binding.Field}' is not usable — "
                            + $"{bindingError}. Fix or remove it in {store.Location}.");
                        continue;
                    }

                    node.Config[binding.Field] = storedField?.DeepClone();
                    resolved++;
                }
            }

            if (!string.Equals(node.Kind, "embedded-primitive", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            var outcome = node.PrimitiveType?.ToLowerInvariant() switch
            {
                "value" => await ResolveValueNodeAsync(node, bindings, cancellationToken),
                "select" => await ResolveSelectNodeAsync(node, definition, bindings, wiredPorts, cancellationToken),
                _ => NodeOutcome.NotApplicable
            };

            switch (outcome.Kind)
            {
                case NodeOutcomeKind.Resolved:
                    resolved++;
                    break;
                case NodeOutcomeKind.Prompted:
                    resolved++;
                    prompted++;
                    break;
                case NodeOutcomeKind.Failed:
                    errors.Add(outcome.Error!);
                    break;
            }

            if (outcome.Binding is { Length: > 0 } bindingName)
            {
                bindings[bindingName] = outcome.BindingValue?.DeepClone();
                bindingsChanged = true;
            }
        }

        // Written once at the end: a run that fails half way through should not leave a partially bound
        // machine behind, and a run that asked nothing should not rewrite the file.
        if (bindingsChanged && errors.Count == 0)
        {
            await store.SaveAsync(bindings, cancellationToken);
        }

        return new BindingPrePassResult(
            Succeeded: errors.Count == 0,
            Errors: errors,
            ResolvedCount: resolved,
            PromptedCount: prompted,
            BindingsChanged: bindingsChanged && errors.Count == 0);
    }

    private async Task<NodeOutcome> ResolveValueNodeAsync(
        PipelineNodeDefinition node,
        IReadOnlyDictionary<string, JsonNode?> bindings,
        CancellationToken cancellationToken)
    {
        var config = ValuePrimitiveConfig.Read(node.Config);

        // Already a constant: a literal is hand-authored and fully portable, and a previous pre-pass on
        // this same definition object has nothing to redo.
        if (config.HasResolved || config.HasLiteral)
        {
            return NodeOutcome.AlreadyResolved;
        }

        if (config.Binding is { Length: > 0 } binding
            && bindings.TryGetValue(binding, out var storedValue))
        {
            if (!Accepts(config, storedValue, out var storedError))
            {
                return NodeOutcome.Fail(
                    $"Value node '{node.Id}': stored binding '{binding}' is not usable — {storedError}. "
                    + $"Fix or remove it in {store.Location}.");
            }

            node.Config[ValuePrimitiveConfig.ResolvedKey] = storedValue?.DeepClone();
            return NodeOutcome.AlreadyResolved;
        }

        if (resolver is { CanResolve: true })
        {
            var request = new ValueRequest(
                NodeId: node.Id,
                Type: config.Type,
                Binding: config.Binding,
                Prompt: config.Prompt,
                Schema: config.Schema,
                Shape: config.Shape,
                Default: config.HasDefault ? config.Default : null);

            ValueResolution resolution;
            try
            {
                resolution = await resolver.ResolveAsync(request, cancellationToken);
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                // A resolver is host code at the edge of the engine — a terminal, later a studio. Letting
                // it take the process down with a stack trace would hide the one thing the operator needs
                // to see, which is which value could not be obtained.
                return NodeOutcome.Fail($"Value node '{node.Id}': the resolver failed — {ex.Message}");
            }

            if (resolution.Error is { Length: > 0 } resolverError)
            {
                return NodeOutcome.Fail($"Value node '{node.Id}': {resolverError}");
            }

            if (resolution.Resolved)
            {
                if (!Accepts(config, resolution.Value, out var answerError))
                {
                    return NodeOutcome.Fail($"Value node '{node.Id}': the answer is not usable — {answerError}.");
                }

                node.Config[ValuePrimitiveConfig.ResolvedKey] = resolution.Value?.DeepClone();
                return NodeOutcome.ResolvedBy(resolution, config.Binding);
            }
        }

        if (config.HasDefault)
        {
            if (!Accepts(config, config.Default, out var defaultError))
            {
                return NodeOutcome.Fail($"Value node '{node.Id}': its default is not usable — {defaultError}.");
            }

            return NodeOutcome.AlreadyResolved;
        }

        return NodeOutcome.Fail(Unresolvable("Value", node.Id, config.Binding));
    }

    private async Task<NodeOutcome> ResolveSelectNodeAsync(
        PipelineNodeDefinition node,
        PipelineDefinition definition,
        IReadOnlyDictionary<string, JsonNode?> bindings,
        IReadOnlySet<(string NodeId, string Port)> wiredPorts,
        CancellationToken cancellationToken)
    {
        var config = SelectPrimitiveConfig.Read(node.Config);

        if (config.HasResolvedCriterion || config.HasWhere)
        {
            return NodeOutcome.AlreadyResolved;
        }

        // A criterion with a producer is a per-frame criterion; resolving it here would freeze something
        // the author deliberately made dynamic. Recorded so the activator knows not to offer it as a live
        // tunable either — a control an edge overwrites every cycle is worse than no control.
        if (wiredPorts.Contains((node.Id, "criterion")))
        {
            node.Config[SelectPrimitiveConfig.CriterionFromEdgeKey] = true;
            return NodeOutcome.NotApplicable;
        }

        if (config.Binding is { Length: > 0 } binding
            && bindings.TryGetValue(binding, out var storedCriterion))
        {
            node.Config[SelectPrimitiveConfig.ResolvedCriterionKey] = storedCriterion?.DeepClone();
            return NodeOutcome.AlreadyResolved;
        }

        if (resolver is { CanResolve: true })
        {
            // Choices are what turn this prompt into a picker — the camera case, and the reason `select`
            // is a primitive at all. An unresolved criterion over a known collection is one composition,
            // not a feature.
            var choices = FindCandidates(node, definition);

            var request = new ValueRequest(
                NodeId: node.Id,
                // With a `by`, the criterion is one property of an element, not the element — so it is not
                // constrained to the collection's element type.
                Type: config.By is { Length: > 0 } ? ControlValueType.String : config.Type,
                Binding: config.Binding,
                Prompt: config.Prompt,
                Choices: choices,
                ChoiceLabelProperty: config.By);

            ValueResolution resolution;
            try
            {
                resolution = await resolver.ResolveAsync(request, cancellationToken);
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                return NodeOutcome.Fail($"Select node '{node.Id}': the resolver failed — {ex.Message}");
            }

            if (resolution.Error is { Length: > 0 } resolverError)
            {
                return NodeOutcome.Fail($"Select node '{node.Id}': {resolverError}");
            }

            if (resolution.Resolved)
            {
                node.Config[SelectPrimitiveConfig.ResolvedCriterionKey] = resolution.Value?.DeepClone();
                return NodeOutcome.ResolvedBy(resolution, config.Binding);
            }
        }

        return NodeOutcome.Fail(Unresolvable("Select", node.Id, config.Binding));
    }

    /// <summary>
    /// The collection a <c>select</c> will narrow, if it is already knowable — which is what lets the
    /// resolver render a picker instead of asking someone to type an identifier they would have to read
    /// off another screen.
    ///
    /// <para>Today that means the producer on <c>items</c> is a <c>value</c> node, whose collection the
    /// pre-pass has already settled (topological order guarantees it ran first). When the discovery module
    /// lands, the same seam takes its output — the only change is where the candidates come from, and
    /// nothing downstream of here moves.</para>
    ///
    /// <para>Null, not empty, when the collection is unknown: an empty list would render as a picker with
    /// nothing in it, which reads like "no cameras found" rather than "not known yet".</para>
    /// </summary>
    private static IReadOnlyList<JsonNode?>? FindCandidates(
        PipelineNodeDefinition node,
        PipelineDefinition definition)
    {
        var incoming = definition.Edges.FirstOrDefault(e =>
            string.Equals(e.To.NodeId, node.Id, StringComparison.OrdinalIgnoreCase)
            && string.Equals(e.To.Port, "items", StringComparison.OrdinalIgnoreCase));

        if (incoming is null)
        {
            return null;
        }

        var producer = definition.Nodes.FirstOrDefault(n =>
            string.Equals(n.Id, incoming.From.NodeId, StringComparison.OrdinalIgnoreCase));

        if (producer is null
            || !string.Equals(producer.Kind, "embedded-primitive", StringComparison.OrdinalIgnoreCase)
            || !string.Equals(producer.PrimitiveType, "value", StringComparison.OrdinalIgnoreCase))
        {
            return null;
        }

        var producerConfig = ValuePrimitiveConfig.Read(producer.Config);
        if (producerConfig.Shape != ControlValueShape.List
            || !producerConfig.TryGetStaticValue(out var collection)
            || collection is not JsonArray items)
        {
            return null;
        }

        return items.Select(item => item?.DeepClone()).ToList();
    }

    private string Unresolvable(string kind, string nodeId, string? binding) =>
        binding is { Length: > 0 }
            ? $"{kind} node '{nodeId}' is unresolved and nothing can answer for it. Set \"{binding}\" in "
              + $"{store.Location}, or run this pipeline once on a terminal that can prompt."
            : $"{kind} node '{nodeId}' is unresolved and declares no binding, so an answer could not even "
              + "be stored. Add a \"binding\" to its config, or write the value into the node.";

    /// <summary>
    /// The one gate every value passes through, wherever it came from. For a list the declared schema
    /// describes an <b>element</b>, so it is applied to each in turn.
    /// </summary>
    private static bool Accepts(ValuePrimitiveConfig config, JsonNode? value, out string? error)
    {
        error = null;

        if (!ControlValueTypes.MatchesShape(config.Shape, config.Type, value))
        {
            error = config.Shape == ControlValueShape.List
                ? $"expected a list of {ControlValueTypes.ToToken(config.Type)}"
                : $"expected {ControlValueTypes.ToToken(config.Type)}";
            return false;
        }

        return JsonSchemaCheck.TryValidateShaped(config.Shape, config.Schema, value, out error);
    }

    /// <summary>The same type + schema gate as a value, for one live module-config field (always single).</summary>
    private static bool AcceptsModuleBinding(ModuleBinding binding, JsonNode? value, out string? error)
    {
        error = null;

        if (!ControlValueTypes.Matches(binding.Type, value))
        {
            error = $"expected {ControlValueTypes.ToToken(binding.Type)}";
            return false;
        }

        return JsonSchemaCheck.TryValidateShaped(ControlValueShape.Single, binding.Schema, value, out error);
    }

    private enum NodeOutcomeKind
    {
        NotApplicable,
        Resolved,
        Prompted,
        Failed
    }

    private sealed record NodeOutcome(
        NodeOutcomeKind Kind,
        string? Error = null,
        string? Binding = null,
        JsonNode? BindingValue = null)
    {
        public static readonly NodeOutcome NotApplicable = new(NodeOutcomeKind.NotApplicable);
        public static readonly NodeOutcome AlreadyResolved = new(NodeOutcomeKind.Resolved);

        public static NodeOutcome Fail(string error) => new(NodeOutcomeKind.Failed, error);

        /// <summary>
        /// A resolver answered. It is only written to the store when the resolver says so — an
        /// environment variable is already durable, and the store is read first, so caching it would let
        /// the first run's value outrank every later change.
        /// </summary>
        public static NodeOutcome ResolvedBy(ValueResolution resolution, string? binding) =>
            resolution.Persist
                ? new NodeOutcome(NodeOutcomeKind.Prompted, Binding: binding, BindingValue: resolution.Value)
                : new NodeOutcome(NodeOutcomeKind.Resolved);
    }
}

public sealed record BindingPrePassResult(
    bool Succeeded,
    IReadOnlyList<string> Errors,
    int ResolvedCount,
    int PromptedCount,
    bool BindingsChanged);
