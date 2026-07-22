namespace Mvf.Engine.Pipelines;

/// <summary>
/// Thrown when a lean pipeline definition cannot be expanded into the rich model —
/// e.g. it references a module that is not in the catalog, or a node is missing an id.
/// These are authoring errors surfaced before validation runs.
/// </summary>
public sealed class PipelineExpansionException(string message) : Exception(message);
