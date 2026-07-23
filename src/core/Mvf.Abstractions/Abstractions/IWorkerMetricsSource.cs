using Mvf.Graph.Execution;

namespace Mvf.Abstractions;

/// <summary>
/// Implemented by an adapter that fronts an out-of-process worker (classifier, transformer) so the
/// engine can read its cross-process counters. Kept here — next to <see cref="IOutOfProcessModuleHost"/>
/// and <see cref="ICheckpointable"/> — so the executor observes worker health without ever depending on
/// stdio, processes, or Python. A node runner forwards for whatever it wraps, exactly as it already does
/// for <see cref="ICheckpointable"/>.
/// </summary>
public interface IWorkerMetricsSource
{
    /// <summary>
    /// Returns the counters as of now, or null when there is no worker behind this node — an in-process
    /// module has nothing to report. Safe to call at any point during a run; call it before the runner is
    /// disposed, since disposal ends the child process.
    /// </summary>
    WorkerMetricsSnapshot? GetWorkerMetrics();
}
