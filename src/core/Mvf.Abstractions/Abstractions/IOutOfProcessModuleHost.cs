namespace Mvf.Abstractions;

/// <summary>
/// A neutral description of a co-located module that runs in another process (Python, Node,
/// out-of-proc .NET). Language/transport specifics live behind <see cref="IOutOfProcessModuleHost"/>;
/// the engine core only ever sees this record.
/// </summary>
/// <param name="ModuleId">The module's id (from its <c>module.json</c>).</param>
/// <param name="Runtime">Runtime tag, e.g. <c>python</c> or <c>node</c>.</param>
/// <param name="EntryPath">Absolute path to the entry script/executable.</param>
/// <param name="WorkingDirectory">Absolute directory the worker is launched in (the module folder).</param>
public sealed record OutOfProcessModuleActivation(
    string ModuleId,
    string Runtime,
    string EntryPath,
    string WorkingDirectory);

/// <summary>
/// Hosts non-.NET modules as co-located, out-of-process workers (M1). Kept behind this seam so
/// the engine's scheduler/activator never depend on stdio, Python, or process specifics —
/// polyglot hosting is an adapter at the edge, per the architecture's "power at the edges" rule.
/// Local only; no network.
/// </summary>
public interface IOutOfProcessModuleHost
{
    /// <summary>
    /// Launches the worker described by <paramref name="activation"/> and returns an
    /// <see cref="IFrameClassifier"/> that forwards each frame to it. The returned classifier
    /// owns the worker process and shuts it down on dispose.
    /// </summary>
    Task<IFrameClassifier> CreateClassifierAsync(
        OutOfProcessModuleActivation activation,
        CancellationToken cancellationToken);
}
