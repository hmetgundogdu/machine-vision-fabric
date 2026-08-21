using System.Text.Json.Nodes;

namespace Mvf.Abstractions;

/// <summary>
/// One diagnostic line emitted by a co-located worker — either a structured <c>log</c> protocol
/// message the module sent on purpose, or a line the child wrote to stderr. Carried up to the engine
/// so a module's own logging reaches the operator instead of being swallowed.
/// </summary>
/// <param name="Level">Severity tag: <c>debug</c>/<c>info</c>/<c>warn</c>/<c>error</c>, or <c>stderr</c> for a raw stderr line.</param>
/// <param name="Message">The log text.</param>
public readonly record struct WorkerLogLine(string Level, string Message);

/// <summary>
/// A neutral description of a co-located module that runs in another process (Python, Node,
/// out-of-proc .NET). Language/transport specifics live behind <see cref="IOutOfProcessModuleHost"/>;
/// the engine core only ever sees this record.
/// </summary>
/// <param name="ModuleId">The module's id (from its <c>module.json</c>).</param>
/// <param name="Runtime">Runtime tag, e.g. <c>python</c> or <c>node</c>.</param>
/// <param name="EntryPath">Absolute path to the entry script/executable.</param>
/// <param name="WorkingDirectory">Absolute directory the worker is launched in (the module folder).</param>
/// <param name="OnLog">Optional sink for the worker's log/stderr lines. Null discards them (M1 behavior).</param>
/// <param name="Config">
/// The node's config block, delivered to the worker once it is ready and before its first frame — the
/// same values an in-process .NET module receives at <c>OpenSession</c>/<c>Create*</c>. Null means the
/// node declared none, and nothing is sent.
///
/// <para>Until this existed, a config on an out-of-process node went nowhere: the engine resolved it,
/// type-checked it, stored it and offered it as a live tunable, and the worker never saw a byte of it.
/// A binding declared on a Python node therefore appeared to work and changed nothing.</para>
///
/// <para>Only delivered to a worker that advertised the <c>configure</c> feature in its handshake; one
/// that did not is left exactly as it was, since it would not answer the message.</para>
/// </param>
public sealed record OutOfProcessModuleActivation(
    string ModuleId,
    string Runtime,
    string EntryPath,
    string WorkingDirectory,
    Action<WorkerLogLine>? OnLog = null,
    JsonNode? Config = null);

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

    /// <summary>
    /// Launches the worker described by <paramref name="activation"/> and returns an
    /// <see cref="IFrameTransformer"/> that sends each frame to it and reads back a new frame from the
    /// data plane. The returned transformer owns the worker process and shuts it down on dispose.
    /// </summary>
    Task<IFrameTransformer> CreateTransformerAsync(
        OutOfProcessModuleActivation activation,
        CancellationToken cancellationToken);

    /// <summary>
    /// Launches the worker described by <paramref name="activation"/> and returns an
    /// <see cref="IFrameAnalyzer"/> that may emit a derived frame, a classification, and structured
    /// inference metadata from one input frame. The returned analyzer owns the worker process and shuts it
    /// down on dispose.
    /// </summary>
    Task<IFrameAnalyzer> CreateAnalyzerAsync(
        OutOfProcessModuleActivation activation,
        CancellationToken cancellationToken);
}
