using Mvf.Graph.Pipelines;
using Mvf.Abstractions;

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
