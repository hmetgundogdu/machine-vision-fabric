using System.Text.Json.Nodes;
using Mvf.Abstractions;

namespace Mvf.Hosting.Worker;

/// <summary>
/// An <see cref="IWorkerChannel"/> that keeps a co-located worker alive across crashes. If the child
/// dies, the next request transparently restarts it, restores its last captured state, and retries once
/// — so a flaky module does not take the run down. Checkpoints flow through here so the supervisor
/// always holds the state to restore with (state travels via the data plane, never base64).
/// </summary>
public sealed class SupervisedWorker : IWorkerChannel, ICheckpointable
{
    private readonly Func<CancellationToken, Task<StdioWorkerProcess>> _spawn;
    private readonly IDataPlane _dataPlane;
    private StdioWorkerProcess _worker;
    private byte[]? _lastState;
    private int _requestId;

    private SupervisedWorker(StdioWorkerProcess worker, Func<CancellationToken, Task<StdioWorkerProcess>> spawn, IDataPlane dataPlane)
    {
        _worker = worker;
        _spawn = spawn;
        _dataPlane = dataPlane;
    }

    public string ModuleId => _worker.ModuleId;

    public static async Task<SupervisedWorker> StartAsync(
        Func<CancellationToken, Task<StdioWorkerProcess>> spawn,
        IDataPlane dataPlane,
        CancellationToken cancellationToken)
    {
        var worker = await spawn(cancellationToken);
        return new SupervisedWorker(worker, spawn, dataPlane);
    }

    public async Task<JsonObject> RequestAsync(JsonObject request, CancellationToken cancellationToken)
    {
        try
        {
            return await _worker.RequestAsync(request, cancellationToken);
        }
        catch (Exception ex) when (ex is not OperationCanceledException && IsWorkerDeath(ex))
        {
            await RestartAsync(cancellationToken);
            return await _worker.RequestAsync(request, cancellationToken); // retry once on the fresh worker
        }
    }

    public async Task<byte[]?> CheckpointAsync(CancellationToken cancellationToken)
    {
        _lastState = await WorkerCheckpoint.CheckpointAsync(_worker, _dataPlane, ++_requestId, cancellationToken);
        return _lastState;
    }

    public async Task RestoreAsync(ReadOnlyMemory<byte> state, CancellationToken cancellationToken)
    {
        await WorkerCheckpoint.RestoreAsync(_worker, _dataPlane, ++_requestId, state, cancellationToken);
        _lastState = state.IsEmpty ? null : state.ToArray();
    }

    private async Task RestartAsync(CancellationToken cancellationToken)
    {
        try { await _worker.DisposeAsync(); } catch { /* already dead */ }

        _worker = await _spawn(cancellationToken);
        if (_lastState is { } state)
        {
            await WorkerCheckpoint.RestoreAsync(_worker, _dataPlane, ++_requestId, state, cancellationToken);
        }
    }

    private bool IsWorkerDeath(Exception ex) =>
        _worker.HasExited || ex is IOException || ex is InvalidOperationException;

    /// <summary>Test hook: crash the current child so the next request exercises recovery.</summary>
    internal void KillCurrentWorkerForTest() => _worker.KillForTest();

    public ValueTask DisposeAsync() => _worker.DisposeAsync();
}
