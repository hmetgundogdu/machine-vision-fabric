using MachineVisionFabric.Contracts.Integrations;

namespace MachineVisionFabric.Sdk;

public static class IntegrationModuleDescriptorBuilder
{
    public static IntegrationModuleDescriptor CreateSource<TOptions>(
        string moduleId,
        string displayName,
        string version,
        string capabilityName,
        string description)
    {
        return Create(
            moduleId,
            displayName,
            version,
            capabilityName,
            IntegrationCapabilityKind.Source,
            typeof(TOptions).FullName ?? typeof(TOptions).Name,
            description);
    }

    public static IntegrationModuleDescriptor CreateGate<TOptions>(
        string moduleId,
        string displayName,
        string version,
        string capabilityName,
        string description)
    {
        return Create(
            moduleId,
            displayName,
            version,
            capabilityName,
            IntegrationCapabilityKind.Gate,
            typeof(TOptions).FullName ?? typeof(TOptions).Name,
            description);
    }

    private static IntegrationModuleDescriptor Create(
        string moduleId,
        string displayName,
        string version,
        string capabilityName,
        IntegrationCapabilityKind kind,
        string schemaType,
        string description)
    {
        return new IntegrationModuleDescriptor
        {
            ModuleId = moduleId,
            DisplayName = displayName,
            Version = version,
            Capabilities =
            [
                new IntegrationCapabilityDescriptor
                {
                    Name = capabilityName,
                    Kind = kind,
                    SchemaType = schemaType,
                    Description = description
                }
            ]
        };
    }
}
