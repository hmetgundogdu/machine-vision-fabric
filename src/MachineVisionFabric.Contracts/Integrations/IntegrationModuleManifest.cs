namespace MachineVisionFabric.Contracts.Integrations;

public sealed class IntegrationModuleManifest
{
    public required string ModuleId { get; init; }

    public required string DisplayName { get; init; }

    public required string Version { get; init; }

    public required string EntryAssembly { get; init; }

    public required string EntryType { get; init; }

    public required IReadOnlyList<IntegrationCapabilityDescriptor> Capabilities { get; init; }
}
