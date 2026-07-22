using System.Diagnostics;
using System.Text.Json.Nodes;

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

    private StdioWorkerProcess(Process process)
    {
        _process = process;
        _stdin = process.StandardInput;
        _stdout = process.StandardOutput;
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

    public static async Task<StdioWorkerProcess> StartAsync(WorkerLaunchInfo info, CancellationToken cancellationToken)
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

        // Drain stderr so a chatty child never blocks on a full pipe.
        _ = Task.Run(async () =>
        {
            try { await process.StandardError.ReadToEndAsync(cancellationToken); } catch { /* ignore */ }
        }, cancellationToken);

        var worker = new StdioWorkerProcess(process);

        // Bound the whole startup handshake (hello + optional readiness) by the startup budget, so a slow
        // model load / device connect can't hang the engine — and a budget overrun is reported as a
        // *startup* failure, not mistaken for a mid-run liveness hang (K8s startup-vs-liveness separation).
        var budget = info.StartupBudget ?? TimeSpan.FromSeconds(30);
        using var startupCts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        startupCts.CancelAfter(budget);

        try
        {
            var hello = await worker.ReadMessageAsync(startupCts.Token)
                ?? throw new WorkerStartupException("Worker exited before sending a hello handshake.");
            if ((string?)hello["type"] != "hello")
            {
                throw new WorkerStartupException("Worker did not send a hello handshake.");
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
            // The budget elapsed (not the caller's cancellation) → a startup timeout.
            await worker.DisposeAsync();
            throw new WorkerStartupException(
                $"Worker '{info.Command}' did not become ready within the startup budget ({budget.TotalSeconds:F0}s).");
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
