using System.Text.Json.Nodes;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Graph.Runtime;
using Mvf.Graph.Values;
using Mvf.Abstractions;
using Mvf.Engine.Values;

namespace Mvf.Engine.Pipelines;

public sealed class PipelineDefinitionValidator : IPipelineDefinitionValidator
{
    public PipelineValidationResult Validate(PipelineDefinition definition)
    {
        ArgumentNullException.ThrowIfNull(definition);

        var issues = new List<PipelineValidationIssue>();
        var nodesById = new Dictionary<string, PipelineNodeDefinition>(StringComparer.OrdinalIgnoreCase);
        var edgeIds = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

        foreach (var node in definition.Nodes)
        {
            ValidateNode(node, issues);

            if (!nodesById.TryAdd(node.Id, node))
            {
                issues.Add(new PipelineValidationIssue
                {
                    Code = "pipeline.node.duplicate-id",
                    Severity = "error",
                    Message = $"Duplicate node id '{node.Id}'.",
                    NodeId = node.Id
                });
            }
        }

        foreach (var edge in definition.Edges)
        {
            if (!edgeIds.Add(edge.Id))
            {
                issues.Add(new PipelineValidationIssue
                {
                    Code = "pipeline.edge.duplicate-id",
                    Severity = "error",
                    Message = $"Duplicate edge id '{edge.Id}'.",
                    EdgeId = edge.Id
                });
            }

            ValidateEdge(edge, nodesById, issues);
        }

        return new PipelineValidationResult
        {
            Issues = issues
        };
    }

    private static void ValidateNode(PipelineNodeDefinition node, ICollection<PipelineValidationIssue> issues)
    {
        if (string.IsNullOrWhiteSpace(node.Id))
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.missing-id",
                Severity = "error",
                Message = "A node is missing an id."
            });
        }

        // Lifecycle contract: a specified activationMode must be a known loading profile (L.1).
        if (node.ActivationMode is { Length: > 0 } mode && !NodeActivationModes.TryParse(mode, out _))
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.invalid-activation-mode",
                Severity = "error",
                Message = $"Node '{node.Id}' has an unknown activationMode '{mode}'. Supported: {NodeActivationModes.Supported}.",
                NodeId = node.Id
            });
        }

        // A specified per-node backpressure policy must be a known one (per-source override).
        if (node.Backpressure is { Length: > 0 } backpressure && !BackpressurePolicies.TryParse(backpressure, out _))
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.invalid-backpressure",
                Severity = "error",
                Message = $"Node '{node.Id}' has an unknown backpressure '{backpressure}'. Supported: {BackpressurePolicies.Supported}.",
                NodeId = node.Id
            });
        }

        // Structural check only. Whether the node may have *this many* instances depends on the module's
        // declared ceiling, which the executor resolves (node → module max), the same split as
        // activationMode and backpressure.
        if (node.Parallelism is { } parallelism && parallelism < 1)
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.invalid-parallelism",
                Severity = "error",
                Message = $"Node '{node.Id}' has parallelism {parallelism}; it must be at least 1.",
                NodeId = node.Id
            });
        }

        if (string.Equals(node.Kind, "integration-module", StringComparison.OrdinalIgnoreCase))
        {
            if (string.IsNullOrWhiteSpace(node.ModuleId))
            {
                issues.Add(new PipelineValidationIssue
                {
                    Code = "pipeline.node.missing-module-id",
                    Severity = "error",
                    Message = $"Integration node '{node.Id}' must declare moduleId.",
                    NodeId = node.Id
                });
            }
        }
        else if (string.Equals(node.Kind, "embedded-primitive", StringComparison.OrdinalIgnoreCase))
        {
            if (string.IsNullOrWhiteSpace(node.PrimitiveType))
            {
                issues.Add(new PipelineValidationIssue
                {
                    Code = "pipeline.node.missing-primitive-type",
                    Severity = "error",
                    Message = $"Primitive node '{node.Id}' must declare primitiveType.",
                    NodeId = node.Id
                });
            }
            else if (string.Equals(node.PrimitiveType, "value", StringComparison.OrdinalIgnoreCase))
            {
                ValidateValueNode(node, issues);
            }
            else if (string.Equals(node.PrimitiveType, "select", StringComparison.OrdinalIgnoreCase))
            {
                ValidateSelectNode(node, issues);
            }
            else if (string.Equals(node.PrimitiveType, "loop", StringComparison.OrdinalIgnoreCase))
            {
                ValidateLoopNode(node, issues);
            }
        }
        else if (string.Equals(node.Kind, "runtime-builtin", StringComparison.OrdinalIgnoreCase))
        {
            if (string.IsNullOrWhiteSpace(node.BuiltinType))
            {
                issues.Add(new PipelineValidationIssue
                {
                    Code = "pipeline.node.missing-builtin-type",
                    Severity = "error",
                    Message = $"Runtime builtin node '{node.Id}' must declare builtinType.",
                    NodeId = node.Id
                });
            }
        }
        else
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.invalid-kind",
                Severity = "error",
                Message = $"Node '{node.Id}' has unsupported kind '{node.Kind}'.",
                NodeId = node.Id
            });
        }

        ValidatePorts(node.Id, node.Inputs, "input", issues);
        ValidatePorts(node.Id, node.Outputs, "output", issues);
        ValidateBindings(node, issues);
    }

    /// <summary>
    /// Live-editable config fields (`bindings`) must be type-safe: each declares a known value type and a
    /// valid schema, so an edit from the CLI can be checked before it re-activates the node. The value
    /// itself lives in <c>config</c> and is checked wherever it enters (pre-pass, live edit), so nothing
    /// value-specific is judged here.
    /// </summary>
    private static void ValidateBindings(PipelineNodeDefinition node, ICollection<PipelineValidationIssue> issues)
    {
        if (node.Bindings.Count == 0)
        {
            return;
        }

        foreach (var binding in ModuleBindings.Read(node.Bindings))
        {
            if (!binding.TypeKnown)
            {
                issues.Add(new PipelineValidationIssue
                {
                    Code = "pipeline.node.invalid-binding-type",
                    Severity = "error",
                    Message = $"Node '{node.Id}' binding '{binding.Field}' declares type '{binding.RawType}'. "
                            + $"Supported: {ControlValueTypes.Supported}.",
                    NodeId = node.Id
                });
            }

            if (!JsonSchemaCheck.TryValidateSchema(binding.Schema, out var schemaError))
            {
                issues.Add(new PipelineValidationIssue
                {
                    Code = "pipeline.node.invalid-binding-schema",
                    Severity = "error",
                    Message = $"Node '{node.Id}' binding '{binding.Field}' has an invalid schema: {schemaError}.",
                    NodeId = node.Id
                });
            }
        }
    }

    /// <summary>
    /// A <c>value</c> node produces one typed value the graph cannot compute, so the things that can be
    /// wrong about it are all knowable statically: an unknown type, a schema that is not a schema, a
    /// literal that does not match either, or no way at all to obtain a value.
    /// </summary>
    private static void ValidateValueNode(PipelineNodeDefinition node, ICollection<PipelineValidationIssue> issues)
    {
        var config = ValuePrimitiveConfig.Read(node.Config);

        if (!config.TypeKnown)
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.invalid-value-type",
                Severity = "error",
                Message = $"Value node '{node.Id}' declares type '{config.RawType}'. Supported: {ControlValueTypes.Supported}.",
                NodeId = node.Id
            });
        }

        if (!config.ShapeKnown)
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.invalid-value-shape",
                Severity = "error",
                Message = $"Value node '{node.Id}' declares shape '{config.RawShape}'. Supported: {ValuePrimitiveConfig.ShapesSupported}.",
                NodeId = node.Id
            });
        }

        var schemaValid = JsonSchemaCheck.TryValidateSchema(config.Schema, out var schemaError);
        if (!schemaValid)
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.invalid-schema",
                Severity = "error",
                Message = $"Value node '{node.Id}' has an invalid schema: {schemaError}.",
                NodeId = node.Id
            });
        }

        // A literal is checked here rather than at run time because it is the one source that is fully
        // known at authoring time — the operator should never see a type error a file could have shown.
        if (config.HasLiteral)
        {
            if (config.TypeKnown && !ControlValueTypes.MatchesShape(config.Shape, config.Type, config.Literal))
            {
                var expected = config.Shape == ControlValueShape.List
                    ? $"a list of '{config.RawType}'"
                    : $"type '{config.RawType}'";

                issues.Add(new PipelineValidationIssue
                {
                    Code = "pipeline.node.literal-type-mismatch",
                    Severity = "error",
                    Message = $"Value node '{node.Id}' has a literal that is not {expected}.",
                    NodeId = node.Id
                });
            }
            else if (schemaValid
                     && !JsonSchemaCheck.TryValidateShaped(config.Shape, config.Schema, config.Literal, out var literalError))
            {
                issues.Add(new PipelineValidationIssue
                {
                    Code = "pipeline.node.literal-type-mismatch",
                    Severity = "error",
                    Message = $"Value node '{node.Id}' has a literal that does not match its schema: {literalError}.",
                    NodeId = node.Id
                });
            }
        }

        // No literal, no binding to look up, no default: nothing a resolver could even be asked about,
        // because a resolver stores its answer under a binding name.
        if (!config.HasLiteral && config.Binding is null && !config.HasDefault)
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.unresolvable-value",
                Severity = "error",
                Message = $"Value node '{node.Id}' declares no literal, binding or default, so it can never "
                        + "produce a value. Add one of them.",
                NodeId = node.Id
            });
        }
    }

    /// <summary>
    /// A <c>select</c> narrows a collection, so the criterion and the elements must be the same type.
    /// The expander always derives both from one declared type; this catches a hand-written or
    /// studio-generated node where they drifted apart.
    /// </summary>
    private static void ValidateSelectNode(PipelineNodeDefinition node, ICollection<PipelineValidationIssue> issues)
    {
        var config = SelectPrimitiveConfig.Read(node.Config);

        if (!config.ModeKnown)
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.select-invalid-mode",
                Severity = "error",
                Message = $"Select node '{node.Id}' has mode '{config.RawMode}'. Supported: {SelectModes.Supported}.",
                NodeId = node.Id
            });
        }

        if (!config.TypeKnown)
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.invalid-value-type",
                Severity = "error",
                Message = $"Select node '{node.Id}' declares type '{config.RawType}'. Supported: {ControlValueTypes.Supported}.",
                NodeId = node.Id
            });
        }

        var items = node.Inputs.FirstOrDefault(p => string.Equals(p.Name, "items", StringComparison.OrdinalIgnoreCase));
        var criterion = node.Inputs.FirstOrDefault(p => string.Equals(p.Name, "criterion", StringComparison.OrdinalIgnoreCase));

        if (items is null
            || criterion is null
            || !ControlPortTypes.TryParse(items.DataType, out _, out var itemType)
            || !ControlPortTypes.TryParse(criterion.DataType, out _, out var criterionType))
        {
            return;
        }

        if (itemType != criterionType)
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.select-type-mismatch",
                Severity = "error",
                Message = $"Select node '{node.Id}' narrows '{items.DataType}' with a criterion of "
                        + $"'{criterion.DataType}'; both must have the same element type.",
                NodeId = node.Id
            });
        }
    }

    /// <summary>
    /// A <c>loop</c> is the graph's iteration authority: it owns the termination policy and carries pause.
    /// Two things can be wrong with it statically: an unknown termination policy, and a <c>count</c> policy
    /// with no positive count — a loop told to stop after N cycles but never told N would never stop.
    /// </summary>
    private static void ValidateLoopNode(PipelineNodeDefinition node, ICollection<PipelineValidationIssue> issues)
    {
        var config = LoopPrimitiveConfig.Read(node.Config);

        if (!config.ModeKnown)
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.loop-invalid-mode",
                Severity = "error",
                Message = $"Loop node '{node.Id}' has mode '{config.RawMode}'. Supported: {LoopModes.Supported}.",
                NodeId = node.Id
            });
        }
        else if (config.Mode == LoopMode.Count && config.Count is not > 0)
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.node.loop-missing-count",
                Severity = "error",
                Message = $"Loop node '{node.Id}' has mode 'count' but no positive 'count'. Add \"count\": N.",
                NodeId = node.Id
            });
        }
    }

    private static void ValidatePorts(
        string nodeId,
        IReadOnlyList<PipelinePortDefinition> ports,
        string direction,
        ICollection<PipelineValidationIssue> issues)
    {
        var names = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var port in ports)
        {
            if (string.IsNullOrWhiteSpace(port.Name))
            {
                issues.Add(new PipelineValidationIssue
                {
                    Code = "pipeline.port.missing-name",
                    Severity = "error",
                    Message = $"Node '{nodeId}' has a {direction} port without a name.",
                    NodeId = nodeId
                });

                continue;
            }

            if (!names.Add(port.Name))
            {
                issues.Add(new PipelineValidationIssue
                {
                    Code = "pipeline.port.duplicate-name",
                    Severity = "error",
                    Message = $"Node '{nodeId}' has duplicate {direction} port '{port.Name}'.",
                    NodeId = nodeId
                });
            }
        }
    }

    private static void ValidateEdge(
        PipelineEdgeDefinition edge,
        IReadOnlyDictionary<string, PipelineNodeDefinition> nodesById,
        ICollection<PipelineValidationIssue> issues)
    {
        if (!nodesById.TryGetValue(edge.From.NodeId, out var fromNode))
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.edge.missing-source-node",
                Severity = "error",
                Message = $"Edge '{edge.Id}' references missing source node '{edge.From.NodeId}'.",
                EdgeId = edge.Id
            });
            return;
        }

        if (!nodesById.TryGetValue(edge.To.NodeId, out var toNode))
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.edge.missing-target-node",
                Severity = "error",
                Message = $"Edge '{edge.Id}' references missing target node '{edge.To.NodeId}'.",
                EdgeId = edge.Id
            });
            return;
        }

        // A loop edge (`save -> cycle`) is a node-level connection that closes the cycle. It moves no value,
        // so there is nothing to type: only that both nodes exist (checked above) and the target really is a
        // loop. No port, channel or data-type checks apply.
        if (string.Equals(edge.Kind, "loop", StringComparison.OrdinalIgnoreCase))
        {
            if (!string.Equals(toNode.PrimitiveType, "loop", StringComparison.OrdinalIgnoreCase))
            {
                issues.Add(new PipelineValidationIssue
                {
                    Code = "pipeline.edge.invalid-loop-target",
                    Severity = "error",
                    Message = $"Edge '{edge.Id}' is a loop edge but its target '{edge.To.NodeId}' is not a loop node.",
                    EdgeId = edge.Id,
                    NodeId = edge.To.NodeId
                });
            }

            return;
        }

        var fromPort = fromNode.Outputs.FirstOrDefault(port => string.Equals(port.Name, edge.From.Port, StringComparison.OrdinalIgnoreCase));
        if (fromPort is null)
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.edge.missing-source-port",
                Severity = "error",
                Message = $"Edge '{edge.Id}' references missing source port '{edge.From.Port}' on node '{edge.From.NodeId}'.",
                EdgeId = edge.Id,
                NodeId = edge.From.NodeId
            });
            return;
        }

        var toPort = toNode.Inputs.FirstOrDefault(port => string.Equals(port.Name, edge.To.Port, StringComparison.OrdinalIgnoreCase));
        if (toPort is null)
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.edge.missing-target-port",
                Severity = "error",
                Message = $"Edge '{edge.Id}' references missing target port '{edge.To.Port}' on node '{edge.To.NodeId}'.",
                EdgeId = edge.Id,
                NodeId = edge.To.NodeId
            });
            return;
        }

        if (!string.Equals(fromPort.Channel, edge.Kind, StringComparison.OrdinalIgnoreCase))
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.edge.source-channel-mismatch",
                Severity = "error",
                Message = $"Edge '{edge.Id}' kind '{edge.Kind}' does not match source port channel '{fromPort.Channel}'.",
                EdgeId = edge.Id,
                NodeId = edge.From.NodeId
            });
        }

        if (!string.Equals(toPort.Channel, edge.Kind, StringComparison.OrdinalIgnoreCase))
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.edge.target-channel-mismatch",
                Severity = "error",
                Message = $"Edge '{edge.Id}' kind '{edge.Kind}' does not match target port channel '{toPort.Channel}'.",
                EdgeId = edge.Id,
                NodeId = edge.To.NodeId
            });
        }

        if (!string.Equals(fromPort.DataType, toPort.DataType, StringComparison.OrdinalIgnoreCase))
        {
            issues.Add(new PipelineValidationIssue
            {
                Code = "pipeline.edge.data-type-mismatch",
                Severity = "error",
                Message = $"Edge '{edge.Id}' connects '{fromPort.DataType}' to '{toPort.DataType}'.",
                EdgeId = edge.Id
            });
        }
    }
}
