namespace Mvf.Hosting.Worker;

/// <summary>
/// A small pool of <b>pre-warmed</b> spare workers for one module, so a restart (or scale-up) does not pay
/// the cold-start — process spawn + model load / device connect (L.2 warmup) — on the hot path. The pool
/// eagerly warms up to a target number of spares; <see cref="AcquireAsync"/> hands out a ready one
/// instantly and tops the pool back up in the background. When the pool is empty it falls back to a direct
/// (cold) spawn, so it is always correct — just not always warm. Local only; no network.
///
/// <para>Standard: the "warm pool" / warm-standby pattern (see <c>docs/module-lifecycle-design.md</c>).
/// A spare is a fully handshaked, ready worker blocked on stdin until acquired; it holds no state, so a
/// caller restores its state (crash recovery) after acquiring — the same as a cold spawn, minus the wait.</para>
/// </summary>
public sealed class WarmWorkerPool : IAsyncDisposable
{
    private readonly Func<CancellationToken, Task<StdioWorkerProcess>> _spawn;
    private readonly int _target;
    private readonly Lock _gate = new();
    private readonly Queue<StdioWorkerProcess> _ready = new();
    private readonly CancellationTokenSource _shutdown = new();
    private int _warming;   // spares currently being pre-warmed (in-flight), so we never overshoot target
    private int _spareHits; // acquires served warm — lets a caller report warm vs cold recovery
    private bool _disposed;

    private WarmWorkerPool(Func<CancellationToken, Task<StdioWorkerProcess>> spawn, int target)
    {
        _spawn = spawn;
        _target = target;
    }

    /// <summary>Number of ready spares currently sitting in the pool (excludes in-flight warmups).</summary>
    public int ReadyCount
    {
        get { lock (_gate) { return _ready.Count; } }
    }

    /// <summary>
    /// Total acquires served from a pre-warmed spare rather than the cold-spawn fallback. A caller can
    /// diff this across an <see cref="AcquireAsync"/> to tell whether that acquire was warm.
    /// </summary>
    public int SpareHits => Volatile.Read(ref _spareHits);

    /// <summary>Creates a pool and eagerly pre-warms up to <paramref name="targetSpares"/> workers.</summary>
    public static async Task<WarmWorkerPool> StartAsync(
        Func<CancellationToken, Task<StdioWorkerProcess>> spawn,
        int targetSpares,
        CancellationToken cancellationToken)
    {
        var pool = new WarmWorkerPool(spawn, Math.Max(0, targetSpares));

        // Warm the initial spares in parallel. A spare that fails to warm is swallowed (the pool just has
        // fewer; the first acquire that finds it empty cold-spawns and surfaces any real error).
        var warming = new List<Task>(pool._target);
        for (var i = 0; i < pool._target; i++)
        {
            warming.Add(pool.WarmOneAsync(cancellationToken));
        }

        await Task.WhenAll(warming);
        return pool;
    }

    /// <summary>
    /// Returns a ready worker: a pre-warmed spare if one is available (then replenishes in the background),
    /// otherwise a fresh cold spawn. A spare that died while idle is discarded.
    /// </summary>
    public async Task<StdioWorkerProcess> AcquireAsync(CancellationToken cancellationToken)
    {
        while (true)
        {
            StdioWorkerProcess? spare = null;
            lock (_gate)
            {
                if (_ready.Count > 0)
                {
                    spare = _ready.Dequeue();
                }
            }

            if (spare is null)
            {
                break;
            }

            if (spare.HasExited)
            {
                await spare.DisposeAsync(); // a spare that died while idle — drop it and try the next
                continue;
            }

            Interlocked.Increment(ref _spareHits);
            _ = ReplenishAsync();
            return spare;
        }

        // Pool empty (spares in use or still warming) → cold spawn. Still correct, just not pre-warmed.
        var worker = await _spawn(cancellationToken);
        _ = ReplenishAsync();
        return worker;
    }

    private async Task WarmOneAsync(CancellationToken cancellationToken)
    {
        try
        {
            var worker = await _spawn(cancellationToken);
            var keep = false;
            lock (_gate)
            {
                if (!_disposed)
                {
                    _ready.Enqueue(worker);
                    keep = true;
                }
            }

            if (!keep)
            {
                await worker.DisposeAsync();
            }
        }
        catch
        {
            // Swallow — a failed spare just means one fewer warm worker.
        }
    }

    private async Task ReplenishAsync()
    {
        lock (_gate)
        {
            if (_disposed || _ready.Count + _warming >= _target)
            {
                return;
            }

            _warming++;
        }

        try
        {
            var worker = await _spawn(_shutdown.Token);
            var keep = false;
            lock (_gate)
            {
                _warming--;
                if (!_disposed)
                {
                    _ready.Enqueue(worker);
                    keep = true;
                }
            }

            if (!keep)
            {
                await worker.DisposeAsync();
            }
        }
        catch
        {
            lock (_gate)
            {
                _warming--;
            }
        }
    }

    public async ValueTask DisposeAsync()
    {
        List<StdioWorkerProcess> toDispose;
        lock (_gate)
        {
            if (_disposed)
            {
                return;
            }

            _disposed = true;
            toDispose = [.. _ready];
            _ready.Clear();
        }

        _shutdown.Cancel();
        foreach (var worker in toDispose)
        {
            try { await worker.DisposeAsync(); } catch { /* best effort */ }
        }

        _shutdown.Dispose();
    }
}
