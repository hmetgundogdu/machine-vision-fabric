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
}
