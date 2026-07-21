using Mvf.Graph.Integrations;

namespace Mvf.Sdk;

public static class IntegrationModuleDescriptorBuilder
{
    private static readonly IReadOnlyList<ModulePortDescriptor> NoPorts = [];

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
            description,
            NoPorts,
            [DataPort("frame", "frame produced by the source module.")]);
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
            description,
            NoPorts,
            [ControlPort("productPresent", "boolean-gate", "Product presence decision emitted by the gate.")]);
    }

    public static IntegrationModuleDescriptor CreateSink<TOptions>(
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
            IntegrationCapabilityKind.Sink,
            typeof(TOptions).FullName ?? typeof(TOptions).Name,
            description,
            [DataPort("frame", "frame received by the sink.")],
            NoPorts);
    }

    public static IntegrationModuleDescriptor CreateProcessor<TOptions>(
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
            IntegrationCapabilityKind.Processor,
            typeof(TOptions).FullName ?? typeof(TOptions).Name,
            description,
            [DataPort("frame", "frame received by the processor.")],
            [DataPort("frame", "frame emitted by the processor when accepted.")]);
    }

    public static IntegrationModuleDescriptor CreateClassifier<TOptions>(
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
            IntegrationCapabilityKind.Classifier,
            typeof(TOptions).FullName ?? typeof(TOptions).Name,
            description,
            [DataPort("frame", "frame whose content is classified.")],
            [ControlPort("class", "classification", "Classification control signal for switch/if routing.")]);
    }

    private static IntegrationModuleDescriptor Create(
        string moduleId,
        string displayName,
        string version,
        string capabilityName,
        IntegrationCapabilityKind kind,
        string schemaType,
        string description,
        IReadOnlyList<ModulePortDescriptor> inputs,
        IReadOnlyList<ModulePortDescriptor> outputs)
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
                    Description = description,
                    Inputs = inputs,
                    Outputs = outputs
                }
            ]
        };
    }

    private static ModulePortDescriptor DataPort(string name, string description)
    {
        return new ModulePortDescriptor
        {
            Name = name,
            Channel = "data",
            DataType = "frame",
            Description = description
        };
    }

    private static ModulePortDescriptor ControlPort(string name, string dataType, string description)
    {
        return new ModulePortDescriptor
        {
            Name = name,
            Channel = "control",
            DataType = dataType,
            Description = description
        };
    }
}
