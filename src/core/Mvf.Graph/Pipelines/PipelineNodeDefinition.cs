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

    /// <summary>
    /// Per-node loading profile override ("resident" | "on-demand"). Null means "not specified" — the
    /// module's declared <c>lifecycle</c> default applies, then resident. Parsed via
    /// <c>NodeActivationModes.TryParse</c>; an unknown value is rejected by the validator.
    /// </summary>
    public string? ActivationMode { get; set; }

    /// <summary>
    /// Per-node data-plane backpressure override ("stall" | "drop") for when this node's output can't be
    /// published because the arena is full. Null means "not specified" — the module's declared
    /// <c>backpressure</c> default applies, then the run-level default. Lets a source pick lossless vs
    /// lossy (folder-replay stalls, live camera drops). Parsed via <c>BackpressurePolicies.TryParse</c>;
    /// an unknown value is rejected by the validator.
    /// </summary>
    public string? Backpressure { get; set; }

    public JsonObject Config { get; set; } = [];

    public IReadOnlyList<PipelinePortDefinition> Inputs { get; set; } = [];

    public IReadOnlyList<PipelinePortDefinition> Outputs { get; set; } = [];
}
