using System.Diagnostics;
using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Graph.Execution;

namespace Mvf.Hosting.Worker;

/// <summary>
/// A co-located module running as a child process, spoken to over stdio with
/// newline-delimited JSON (see protocol/README.md). Local only — no network.
///
/// One request in flight at a time (guarded by a lock); the engine drives one node
/// per cycle, so this matches the scheduling model. Frame data is carried inline for
/// M1; the shared-memory data plane (M2) replaces it with a handle.
/// </summary>
public sealed class StdioWorkerProcess : IWorkerChannel
{
    private readonly Process _process;
    private readonly StreamWriter _stdin;
    private readonly StreamReader _stdout;
    private readonly SemaphoreSlim _gate = new(1, 1);
    private readonly Action<WorkerLogLine>? _onLog;

    // CPU/memory sampling state. Throttled (~500ms) so the engine can poll it every cycle for free and the
    // CPU% is measured over a real window instead of a noisy sub-millisecond slice.
    private static readonly long SampleThrottleTicks = Stopwatch.Frequency / 2;
    private readonly object _sampleLock = new();
    private long _lastSampleTimestamp;         // Stopwatch ticks; 0 = never sampled
    private TimeSpan _lastCpuTime;
    private WorkerResourceSample? _lastSample;

    private StdioWorkerProcess(Process process, Action<WorkerLogLine>? onLog)
    {
        _process = process;
        _stdin = process.StandardInput;
        _stdout = process.StandardOutput;
        _onLog = onLog;
    }

    /// <summary>Reads a <c>log</c> protocol message's level/message and forwards it to the sink.</summary>
    private void ForwardLog(JsonObject message)
    {
        if (_onLog is null)
        {
            return;
        }

        var level = (string?)message["level"] ?? "info";
        var text = (string?)message["message"] ?? string.Empty;
        try { _onLog(new WorkerLogLine(level, text)); }
        catch { /* a logging sink must never break the run */ }
    }

    public string ModuleId { get; private set; } = string.Empty;

    /// <summary>True once the child process has exited — the signal a supervisor uses to restart it.</summary>
    public bool HasExited
    {
        get
        {
            try { return _process.HasExited; }
            catch { return true; }
        }
    }

    /// <summary>
    /// Current CPU/memory of the child process, throttled to ~500ms. CPU% is the child's processor time
    /// since the previous sample over the wall-clock elapsed, normalised to all cores (0–100); the first
    /// sample has no prior reading so it reports 0. Returns null once the child has exited.
    /// </summary>
    public WorkerResourceSample? SampleResources()
    {
        lock (_sampleLock)
        {
            var now = Stopwatch.GetTimestamp();
            if (_lastSample is { } cached && _lastSampleTimestamp != 0 && now - _lastSampleTimestamp < SampleThrottleTicks)
            {
                return cached;
            }

            try
            {
                if (_process.HasExited)
                {
                    _lastSample = null;
                    return null;
                }

                _process.Refresh();   // Process caches these — refresh for a live reading.
                var workingSet = _process.WorkingSet64;
                long peak = 0;
                try { peak = _process.PeakWorkingSet64; } catch { /* not tracked on this platform */ }
                var cpuTime = _process.TotalProcessorTime;

                double cpuPercent = 0;
                if (_lastSampleTimestamp != 0)
                {
                    var wall = Stopwatch.GetElapsedTime(_lastSampleTimestamp, now);
                    if (wall > TimeSpan.Zero)
                    {
                        var cores = Math.Max(1, Environment.ProcessorCount);
                        cpuPercent = Math.Clamp(
                            (cpuTime - _lastCpuTime).TotalMilliseconds / wall.TotalMilliseconds / cores * 100.0, 0, 100);
                    }
                }

                _lastCpuTime = cpuTime;
                _lastSampleTimestamp = now;
                _lastSample = new WorkerResourceSample
                {
                    WorkingSetBytes = workingSet,
                    PeakWorkingSetBytes = peak,
                    CpuPercent = cpuPercent
                };
                return _lastSample;
            }
            catch
            {
                // Racing the child's exit, or a host that refuses the query — keep the last good reading.
                return _lastSample;
            }
        }
    }

    /// <summary>Test hook: forcibly kills the child to simulate a crash.</summary>
    internal void KillForTest()
    {
        try
        {
            _process.Kill(entireProcessTree: true);
            _process.WaitForExit(2000);
        }
        catch { /* already gone */ }
    }

    /// <summary>Convenience overload for callers that do not consume worker logs (tests, warm spares).</summary>
    public static Task<StdioWorkerProcess> StartAsync(WorkerLaunchInfo info, CancellationToken cancellationToken) =>
        StartAsync(info, onLog: null, cancellationToken);

    public static async Task<StdioWorkerProcess> StartAsync(
        WorkerLaunchInfo info,
        Action<WorkerLogLine>? onLog,
        CancellationToken cancellationToken)
    {
        var psi = new ProcessStartInfo
        {
            FileName = info.Command,
            WorkingDirectory = info.WorkingDirectory,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
        };
        foreach (var arg in info.Args)
        {
            psi.ArgumentList.Add(arg);
        }
        if (!string.IsNullOrEmpty(info.PythonPath))
        {
            psi.Environment["PYTHONPATH"] = info.PythonPath;
        }
        if (!string.IsNullOrEmpty(info.ArenaPath))
        {
            psi.Environment["MVF_ARENA_PATH"] = info.ArenaPath;
        }
        if (info.Environment is not null)
        {
            foreach (var (key, value) in info.Environment)
            {
                psi.Environment[key] = value;
            }
        }

        var process = Process.Start(psi)
            ?? throw new InvalidOperationException($"Failed to start worker '{info.Command}'.");

        // Capture stderr (bounded to the tail) rather than discard it: when a worker dies before the hello
        // handshake, its stderr is the only record of *why* — a Python traceback, a "python was not found"
        // stub message, a missing import. Draining also keeps a chatty child from blocking on a full pipe.
        // Bounded so a long, noisy run can't grow it without limit; the tail is where the error lands.
        var stderr = new System.Text.StringBuilder();
        const int stderrCap = 8192;
        var stderrDrain = Task.Run(async () =>
        {
            try
            {
                string? line;
                while ((line = await process.StandardError.ReadLineAsync(cancellationToken)) is not null)
                {
                    lock (stderr)
                    {
                        stderr.Append(line).Append('\n');
                        if (stderr.Length > stderrCap) stderr.Remove(0, stderr.Length - stderrCap);
                    }

                    // Also forward each stderr line upstream so a module's own logging/traceback reaches
                    // the operator live — not just the bounded tail kept for a startup-failure message.
                    if (onLog is not null && line.Length > 0)
                    {
                        try { onLog(new WorkerLogLine("stderr", line)); }
                        catch { /* a logging sink must never break the drain */ }
                    }
                }
            }
            catch { /* ignore */ }
        }, cancellationToken);

        var worker = new StdioWorkerProcess(process, onLog);

        // Builds a " (exit code N; stderr: …)" suffix for a startup-failure message. Waits briefly for the
        // drain to flush once the child is gone, so the child's own error text makes it into the exception.
        async Task<string> StartupDiagnosticsAsync()
        {
            try { await stderrDrain.WaitAsync(TimeSpan.FromSeconds(1), cancellationToken); }
            catch { /* still running, or cancelled — report whatever was captured so far */ }

            var parts = new List<string>();
            try { if (process.HasExited) parts.Add($"exit code {process.ExitCode}"); }
            catch { /* racing the exit — skip */ }

            string tail;
            lock (stderr) tail = stderr.ToString().Trim();
            if (tail.Length > 0) parts.Add($"stderr: {tail}");

            return parts.Count > 0 ? $" ({string.Join("; ", parts)})" : string.Empty;
        }

        // Bound the whole startup handshake (hello + optional readiness) by the startup budget, so a slow
        // model load / device connect can't hang the engine — and a budget overrun is reported as a
        // *startup* failure, not mistaken for a mid-run liveness hang (K8s startup-vs-liveness separation).
        var budget = info.StartupBudget ?? TimeSpan.FromSeconds(30);
        using var startupCts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        startupCts.CancelAfter(budget);

        try
        {
            var hello = await worker.ReadMessageAsync(startupCts.Token);
            if (hello is null)
            {
                throw new WorkerStartupException(
                    "Worker exited before sending a hello handshake." + await StartupDiagnosticsAsync());
            }
            if ((string?)hello["type"] != "hello")
            {
                throw new WorkerStartupException(
                    "Worker did not send a hello handshake." + await StartupDiagnosticsAsync());
            }
            worker.ModuleId = (string?)hello["moduleId"] ?? string.Empty;

            // Readiness (sd_notify-style): a worker that warms up asynchronously says "ready": false in its
            // hello and sends a separate `ready` when warm. Absent/true → ready now (backward compatible).
            if (hello["ready"] is JsonValue readyValue && readyValue.TryGetValue<bool>(out var ready) && !ready)
            {
                await worker.WaitForReadyAsync(startupCts.Token);
            }
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        {
            // The budget elapsed (not the caller's cancellation) → a startup timeout. Read diagnostics
            // before disposing, since Dispose kills the child and rewrites its exit code.
            var diagnostics = await StartupDiagnosticsAsync();
            await worker.DisposeAsync();
            throw new WorkerStartupException(
                $"Worker '{info.Command}' did not become ready within the startup budget ({budget.TotalSeconds:F0}s).{diagnostics}");
        }
        catch
        {
            await worker.DisposeAsync();
            throw;
        }

        return worker;
    }

    /// <summary>Waits for the child's <c>ready</c> signal after warmup, skipping log lines.</summary>
    private async Task WaitForReadyAsync(CancellationToken cancellationToken)
    {
        while (true)
        {
            var message = await ReadMessageAsync(cancellationToken)
                ?? throw new WorkerStartupException("Worker exited during warmup before signaling ready.");
            var type = (string?)message["type"];
            if (type == "log")
            {
                ForwardLog(message);
                continue;
            }
            if (type == "ready")
            {
                return;
            }

            throw new WorkerStartupException($"Worker sent '{type}' while the engine was waiting for readiness.");
        }
    }

    /// <summary>Send one request and read the matching response (skipping log lines).</summary>
    public async Task<JsonObject> RequestAsync(JsonObject request, CancellationToken cancellationToken)
    {
        await _gate.WaitAsync(cancellationToken);
        try
        {
            await _stdin.WriteLineAsync(request.ToJsonString());
            await _stdin.FlushAsync(cancellationToken);

            while (true)
            {
                var message = await ReadMessageAsync(cancellationToken)
                    ?? throw new InvalidOperationException("Worker closed the connection before responding.");
                if ((string?)message["type"] == "log")
                {
                    ForwardLog(message);
                    continue;
                }
                return message;
            }
        }
        finally
        {
            _gate.Release();
        }
    }

    private async Task<JsonObject?> ReadMessageAsync(CancellationToken cancellationToken)
    {
        while (true)
        {
            var line = await _stdout.ReadLineAsync(cancellationToken);
            if (line is null)
            {
                return null;
            }
            line = line.Trim();
            if (line.Length == 0)
            {
                continue;
            }
            return JsonNode.Parse(line) as JsonObject
                ?? throw new InvalidOperationException($"Worker sent a non-object message: {line}");
        }
    }

    public async ValueTask DisposeAsync()
    {
        try
        {
            if (!_process.HasExited)
            {
                await _stdin.WriteLineAsync("{\"type\":\"shutdown\"}");
                await _stdin.FlushAsync();
                _stdin.Close();
                if (!_process.WaitForExit(2000))
                {
                    _process.Kill(entireProcessTree: true);
                }
            }
        }
        catch
        {
            try { _process.Kill(entireProcessTree: true); } catch { /* ignore */ }
        }
        finally
        {
            _process.Dispose();
            _gate.Dispose();
        }
    }
}
