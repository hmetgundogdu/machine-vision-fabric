namespace Mvf.Graph.Integrations;

public sealed class IntegrationCapabilityDescriptor
{
    public required string Name { get; init; }

    public required IntegrationCapabilityKind Kind { get; init; }

    public required string SchemaType { get; init; }

    public string Description { get; init; } = string.Empty;

    public IReadOnlyList<ModulePortDescriptor> Inputs { get; init; } = [];

    public IReadOnlyList<ModulePortDescriptor> Outputs { get; init; } = [];
}
