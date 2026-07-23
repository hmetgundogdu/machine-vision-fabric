namespace Mvf.Graph.Integrations;

/// <summary>
/// The one manifest format for every module, regardless of language: <c>module.json</c>.
/// Minimal by design — ports are derived from <see cref="Kind"/>, so a module never
/// re-declares them. The .NET entry type is auto-discovered from the assembly.
/// </summary>
public sealed class ModuleManifest
{
    public required string Id { get; init; }

    public required string Name { get; init; }

    public required string Version { get; init; }

    /// <summary>source | processor | classifier | gate | sink — determines the port contract.</summary>
    public required IntegrationCapabilityKind Kind { get; init; }

    /// <summary>dotnet | python | node.</summary>
    public string Runtime { get; init; } = "dotnet";

    /// <summary>For <c>dotnet</c>: the assembly file (e.g. <c>Foo.dll</c>). For a process runtime: the script/entry.</summary>
    public required string Entry { get; init; }

    /// <summary>
    /// Module-declared default loading profile ("resident" | "on-demand"). Null → resident. A pipeline
    /// node's <c>activationMode</c> overrides it. Part of the lifecycle contract
    /// (see <c>docs/module-lifecycle-design.md</c>); richer lifecycle fields (readiness, warmup budget)
    /// land in later L-track slices.
    /// </summary>
    public string? Lifecycle { get; init; }

    /// <summary>
    /// Module-declared default data-plane backpressure policy ("stall" | "drop") when this module's output
    /// can't be published because the arena is full. Null → the run-level default. A pipeline node's
    /// <c>backpressure</c> overrides it. Lets a live-camera source default to <c>drop</c> and a
    /// folder-replay source to <c>stall</c> without per-pipeline wiring.
    /// </summary>
    public string? Backpressure { get; init; }

    /// <summary>
    /// The most instances of this module the engine may run concurrently. Null → 1, so nothing is ever
    /// replicated by accident. Raising it is the module author's assertion that the module keeps no state
    /// across frames — the engine cannot verify that, and getting it wrong means N silently diverging
    /// states. Leave it at 1 for anything stateful, and for a runtime that parallelises better internally
    /// (ONNX Runtime intra-op threads, GPU batching), where N processes would just mean N model copies.
    /// </summary>
    public int? MaxParallelism { get; init; }
}
