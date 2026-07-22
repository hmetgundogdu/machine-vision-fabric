using System.Text.Json.Nodes;

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

    /// <summary>Sends one request and returns the matching response (log lines skipped).</summary>
    Task<JsonObject> RequestAsync(JsonObject request, CancellationToken cancellationToken);
}
