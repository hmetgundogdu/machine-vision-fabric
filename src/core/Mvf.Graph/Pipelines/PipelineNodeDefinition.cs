using System.Text.Json.Nodes;

namespace Mvf.Graph.Pipelines;

public sealed class PipelineNodeDefinition
{
    public string Id { get; set; } = "node";

    public string DisplayName { get; set; } = "Node";

    public string Kind { get; set; } = "integration-module";

    public string Category { get; set; } = "compute";

    public string? ModuleId { get; set; }

    public string? PrimitiveType { get; set; }

    public string? BuiltinType { get; set; }

    public string? Capability { get; set; }

    public string ActivationMode { get; set; } = "resident";

    public JsonObject Config { get; set; } = [];

    public IReadOnlyList<PipelinePortDefinition> Inputs { get; set; } = [];

    public IReadOnlyList<PipelinePortDefinition> Outputs { get; set; } = [];
}
