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
    private readonly WarmWorkerPool? _pool;
    private StdioWorkerProcess _worker;
    private byte[]? _lastState;
    private int _requestId;

    private SupervisedWorker(
        StdioWorkerProcess worker,
        Func<CancellationToken, Task<StdioWorkerProcess>> spawn,
        IDataPlane dataPlane,
        WarmWorkerPool? pool)
    {
        _worker = worker;
        _spawn = spawn;
        _dataPlane = dataPlane;
        _pool = pool;
    }

    public string ModuleId => _worker.ModuleId;

    /// <summary>
    /// Starts a supervised worker. When <paramref name="pool"/> is given, the initial worker and every
    /// restart come from the pre-warmed pool (no cold-start on the recovery hot path); the pool is owned
    /// here and disposed with the supervisor. Without a pool, restarts cold-spawn (original behavior).
    /// </summary>
    public static async Task<SupervisedWorker> StartAsync(
        Func<CancellationToken, Task<StdioWorkerProcess>> spawn,
        IDataPlane dataPlane,
        CancellationToken cancellationToken,
        WarmWorkerPool? pool = null)
    {
        var worker = pool is not null ? await pool.AcquireAsync(cancellationToken) : await spawn(cancellationToken);
        return new SupervisedWorker(worker, spawn, dataPlane, pool);
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

        // A pre-warmed spare (if pooled) skips the cold-start; restore its state and it is ready to retry.
        _worker = _pool is not null ? await _pool.AcquireAsync(cancellationToken) : await _spawn(cancellationToken);
        if (_lastState is { } state)
        {
            await WorkerCheckpoint.RestoreAsync(_worker, _dataPlane, ++_requestId, state, cancellationToken);
        }
    }

    private bool IsWorkerDeath(Exception ex) =>
        _worker.HasExited || ex is IOException || ex is InvalidOperationException;

    /// <summary>Test hook: crash the current child so the next request exercises recovery.</summary>
    internal void KillCurrentWorkerForTest() => _worker.KillForTest();

    public async ValueTask DisposeAsync()
    {
        await _worker.DisposeAsync();
        if (_pool is not null)
        {
            await _pool.DisposeAsync();
        }
    }
}
