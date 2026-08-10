using System.Text.Json.Nodes;
using Mvf.Graph.Execution;

namespace Mvf.Hosting.Worker;

/// <summary>
/// The request/response channel to a co-located worker over stdio. Abstracts the concrete transport so a
/// worker-backed runner can talk to either a raw <see cref="StdioWorkerProcess"/> or a
/// <see cref="SupervisedWorker"/> that transparently restarts a crashed child.
/// </summary>
public interface IWorkerChannel : IAsyncDisposable
{
    /// <summary>The module id from the child's <c>hello</c> handshake.</summary>
    string ModuleId { get; }

    /// <summary>
    /// How often this channel had to replace a dead child. A plain channel cannot restart, so it reports
    /// <see cref="WorkerRestartStats.None"/>; a <see cref="SupervisedWorker"/> reports its real history,
    /// which is what makes an absorbed crash visible in the execution report.
    /// </summary>
    WorkerRestartStats RestartStats => WorkerRestartStats.None;

    /// <summary>
    /// Current CPU/memory of the child process backing this channel, or null when there is no measurable
    /// process (or it has exited). The engine polls it through <see cref="Mvf.Abstractions.IWorkerMetricsSource"/>
    /// like the restart history, so per-node resource use is observed without depending on processes here.
    /// </summary>
    WorkerResourceSample? SampleResources() => null;

    /// <summary>Sends one request and returns the matching response (log lines skipped).</summary>
    Task<JsonObject> RequestAsync(JsonObject request, CancellationToken cancellationToken);

    /// <summary>
    /// Whether the child advertised the <c>configure</c> feature in its <c>hello</c>. False by default,
    /// which is the safe answer for any channel that does not know: an unknown message type is *ignored*
    /// by an older SDK's dispatch loop rather than refused, so sending one to a worker that never claimed
    /// the feature would leave the engine waiting for a reply that is never coming.
    /// </summary>
    bool SupportsConfigure => false;

    /// <summary>
    /// Hands the running child a new config block (protocol <c>configure</c>). Only valid when
    /// <see cref="SupportsConfigure"/> is true.
    /// </summary>
    Task ConfigureAsync(JsonNode? config, CancellationToken cancellationToken) =>
        throw new NotSupportedException(
            $"Worker '{ModuleId}' cannot be configured while running.");
}
