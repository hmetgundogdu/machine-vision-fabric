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

    /// <summary>
    /// How many instances of this node to run concurrently in pipelined mode. Null or 1 means one, which
    /// is the only safe default: replicating a node only works when it holds no state across frames, since
    /// N instances would otherwise diverge into N states and recovery persists exactly one per node.
    /// A module opts in by declaring <c>maxParallelism</c>; the validator rejects a node asking for more
    /// than its module allows. Ignored by the serial executor, which always runs one instance.
    /// </summary>
    public int? Parallelism { get; set; }

    public JsonObject Config { get; set; } = [];

    /// <summary>
    /// Config fields the operator may edit live from the CLI, each declaring its type (and optionally a
    /// schema + a persistence binding). The field's current value lives in <see cref="Config"/>; this map
    /// only says "this field is tunable, and here is how to type-check it". A change re-activates the node
    /// with the new value — modules read config at activation, so re-opening is how the new value takes
    /// effect. Empty for a node with no live-editable config.
    /// </summary>
    public JsonObject Bindings { get; set; } = [];

    public IReadOnlyList<PipelinePortDefinition> Inputs { get; set; } = [];

    public IReadOnlyList<PipelinePortDefinition> Outputs { get; set; } = [];
}
