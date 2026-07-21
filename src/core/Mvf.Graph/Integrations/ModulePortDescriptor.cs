namespace Mvf.Graph.Integrations;

public sealed class ModulePortDescriptor
{
    public required string Name { get; init; }

    public required string Channel { get; init; }

    public required string DataType { get; init; }

    public bool Required { get; init; } = true;

    public bool AllowMultipleEdges { get; init; }

    public string Description { get; init; } = string.Empty;
}
