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

    // Recovery history. Restarts are transparent to the caller by design, so without this the crash
    // would leave no trace at all — this is what the execution report surfaces (M3 observability).
    private int _restarts;
    private int _warmRestarts;
    private DateTime? _lastRestartUtc;
    private string? _lastRestartReason;

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

    public WorkerRestartStats RestartStats =>
        new(_restarts, _warmRestarts, _lastRestartUtc, _lastRestartReason);

    /// <summary>Samples the current child. A restart swaps <see cref="_worker"/> for a fresh process, so its
    /// CPU baseline resets on its own — the first sample after recovery simply reports 0% CPU.</summary>
    public Mvf.Graph.Execution.WorkerResourceSample? SampleResources() => _worker.SampleResources();

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
            await RestartAsync(ex.Message, cancellationToken);
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

    private async Task RestartAsync(string reason, CancellationToken cancellationToken)
    {
        try { await _worker.DisposeAsync(); } catch { /* already dead */ }

        // A pre-warmed spare (if pooled) skips the cold-start; restore its state and it is ready to retry.
        var spareHitsBefore = _pool?.SpareHits ?? 0;
        _worker = _pool is not null ? await _pool.AcquireAsync(cancellationToken) : await _spawn(cancellationToken);

        _restarts++;
        if (_pool is not null && _pool.SpareHits > spareHitsBefore)
        {
            _warmRestarts++;   // recovery skipped the cold-start (L.4)
        }

        _lastRestartUtc = DateTime.UtcNow;
        _lastRestartReason = reason;

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
