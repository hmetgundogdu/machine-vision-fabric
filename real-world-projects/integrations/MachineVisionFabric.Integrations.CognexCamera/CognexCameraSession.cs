using System.Net.WebSockets;
using System.Text;
using System.Text.Json;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Integrations.CognexCamera;

/// <summary>
/// Cognex In-Sight HMI WebSocket frame source session.
///
/// Modes:
///   passive-listen  — listens for resultChanged events; camera triggers externally (e.g. PLC)
///   manual-trigger  — sends a software trigger on a fixed interval
///
/// Runs continuously until the pipeline executor cancels the token.
/// No MaxFrames limit — the dark-frame filter and dataset writer downstream decide what gets saved.
/// </summary>
public sealed class CognexCameraSession : BackgroundFrameSourceSession
{
    private readonly SemaphoreSlim   _hmiSendLock  = new(1, 1);
    private readonly SemaphoreSlim   _hmiFetchLock = new(1, 1);
    private readonly Dictionary<int, TaskCompletionSource<JsonElement>> _pending = [];
    private readonly object          _pendingSync  = new();
    private readonly CognexCameraOptions _opts;

    private ClientWebSocket?  _ws;
    private HttpClient?       _http;
    private CancellationTokenSource? _hmiCts;
    private Task?             _receiveTask;
    private string?           _sessionPath;
    private int               _reqId = 100;

    public CognexCameraSession(CognexCameraOptions options)
        : base(declaredCameraCount: 1, estimatedFrameCount: null, boundedCapacity: options.BoundedCapacity)
    {
        _opts = options;
        StartBackgroundProducer(ProduceFramesAsync);
    }

    // ── Main producer ────────────────────────────────────────────────────────

    private async Task ProduceFramesAsync(CancellationToken ct)
    {
        try
        {
            if (_opts.StartupDelayMs > 0)
            {
                Log($"Startup delay: {_opts.StartupDelayMs} ms");
                await Task.Delay(_opts.StartupDelayMs, ct);
            }

            await ConnectAsync(ct);

            if (_opts.AcquisitionMode.Equals("manual-trigger", StringComparison.OrdinalIgnoreCase))
                await RunManualTriggerLoopAsync(ct);
            else
                await RunPassiveListenLoopAsync(ct);
        }
        catch (OperationCanceledException)
        {
            // normal shutdown
        }
        catch (Exception ex)
        {
            Log($"[ERROR] {ex.Message}");
        }
        finally
        {
            await SafeDisconnectAsync();
            _hmiSendLock.Dispose();
            _hmiFetchLock.Dispose();
        }
    }

    // ── Connection ───────────────────────────────────────────────────────────

    private async Task ConnectAsync(CancellationToken ct)
    {
        await SafeDisconnectAsync();

        var scheme = _opts.HmiUseTls ? "wss" : "ws";
        var path   = _opts.HmiWebSocketPath.StartsWith('/') ? _opts.HmiWebSocketPath : "/" + _opts.HmiWebSocketPath;
        var uri    = new Uri($"{scheme}://{_opts.IpAddress}:{_opts.HmiPort}{path}");

        Log($"Connecting: {uri}");
        _hmiCts = CancellationTokenSource.CreateLinkedTokenSource(ct);
        _ws = new ClientWebSocket();
        await _ws.ConnectAsync(uri, ct);
        Log("WebSocket connected");

        var httpScheme = _opts.HmiUseTls ? "https" : "http";
        _http = new HttpClient
        {
            BaseAddress = new Uri($"{httpScheme}://{_opts.IpAddress}:{_opts.HmiPort}"),
            Timeout = TimeSpan.FromMilliseconds(Math.Max(1000, _opts.ResponseTimeoutMs))
        };

        _receiveTask = Task.Run(() => ReceiveLoopAsync(_hmiCts.Token), _hmiCts.Token);

        await SendAsync("get", "system/info",     null, ct);
        await SendAsync("get", "system/job",      null, ct);
        await OpenSessionAsync(ct);
    }

    private async Task OpenSessionAsync(CancellationToken ct)
    {
        var payload = new object[]
        {
            new Dictionary<string, object?>
            {
                ["$type"]                = "HmiSessionInfo",
                ["enableQueuedResults"]  = true,
                ["cellNames"]            = null,
                ["includeCustomView"]    = true,
                ["includeEasyView"]      = true
            }
        };

        var resp = await SendAsync("post", "system/openSession", payload, ct);

        var sp = resp.TryGetProperty("body", out var b) && b.ValueKind == JsonValueKind.String
            ? b.GetString()
            : null;

        if (string.IsNullOrWhiteSpace(sp) || sp.Contains("Access denied", StringComparison.OrdinalIgnoreCase))
            throw new InvalidOperationException($"Cognex HMI openSession failed: {sp ?? "no session path returned"}");

        _sessionPath = sp;
        Log($"Session opened: {_sessionPath}");

        await SendAsync("post",   $"{_sessionPath}/login",         BuildLoginPayload(), ct);
        await SendAsync("listen", $"{_sessionPath}/resultChanged", null, ct);
        await SendAsync("listen", "system/stateChanged",           null, ct);
        Log("HMI listeners registered");
    }

    private async Task ReopenSessionAsync(CancellationToken ct)
    {
        if (!string.IsNullOrWhiteSpace(_sessionPath))
        {
            try { await SendAsync("post", $"{_sessionPath}/dispose", Array.Empty<object>(), ct, timeoutMs: 700); }
            catch { /* best-effort */ }
        }
        await OpenSessionAsync(ct);
        Log($"Session reopened: {_sessionPath}");
    }

    // ── Acquisition loops ────────────────────────────────────────────────────

    private async Task RunManualTriggerLoopAsync(CancellationToken ct)
    {
        Log("Manual-trigger mode started");
        while (!ct.IsCancellationRequested)
        {
            await TriggerAsync(ct);
            if (_opts.ManualTriggerIntervalMs > 0)
                await Task.Delay(_opts.ManualTriggerIntervalMs, ct);
        }
    }

    private async Task RunPassiveListenLoopAsync(CancellationToken ct)
    {
        Log($"Passive-listen mode started (ready interval: {_opts.HmiReadyIntervalMs} ms)");
        while (!ct.IsCancellationRequested)
        {
            if (!string.IsNullOrWhiteSpace(_sessionPath))
                await TrySendReadyAsync(ct);

            await Task.Delay(_opts.HmiReadyIntervalMs > 0 ? _opts.HmiReadyIntervalMs : 1000, ct);
        }
    }

    private async Task TriggerAsync(CancellationToken ct)
    {
        for (var attempt = 0; attempt <= _opts.ManualTriggerRetryCount; attempt++)
        {
            try
            {
                if (attempt > 0 || _opts.ReopenSessionBeforeManualTrigger)
                    await ReopenSessionAsync(ct);

                await SendAsync("post", $"{_sessionPath}/trigger", Array.Empty<object>(), ct);
                await FetchAndEnqueueFrameAsync(ct);
                return;
            }
            catch (OperationCanceledException) { throw; }
            catch (Exception ex) when (attempt < _opts.ManualTriggerRetryCount)
            {
                Log($"[WARN] Trigger attempt {attempt + 1} failed: {ex.Message}. Retrying…");
                await Task.Delay(200, ct);
            }
        }
    }

    private async Task TrySendReadyAsync(CancellationToken ct)
    {
        try { await SendAsync("post", $"{_sessionPath}/ready", Array.Empty<object>(), ct, timeoutMs: 800); }
        catch (OperationCanceledException) { throw; }
        catch (Exception ex) { Log($"[WARN] ready signal: {ex.Message}"); }
    }

    // ── Frame fetch ──────────────────────────────────────────────────────────

    private async Task FetchAndEnqueueFrameAsync(CancellationToken ct)
    {
        await _hmiFetchLock.WaitAsync(ct);
        try
        {
            var query = string.IsNullOrWhiteSpace(_opts.HmiImageQuery)
                ? $"{_sessionPath}/image"
                : $"{_sessionPath}/{_opts.HmiImageQuery}";

            var resp = await _http!.GetAsync(query, ct);
            resp.EnsureSuccessStatusCode();

            var bytes = await resp.Content.ReadAsByteArrayAsync(ct);
            if (bytes.Length == 0) return;

            var ct2 = resp.Content.Headers.ContentType?.MediaType ?? "image/jpeg";
            var envelope = FrameEnvelopeFactory.FromBytes(
                cameraId:    _opts.CameraId,
                sequenceNumber: 0,
                fileName:    $"frame-{DateTime.UtcNow:HHmmss-fff}.jpg",
                payload:     bytes,
                contentType: ct2);

            await PublishAsync(envelope, ct);

            if (_opts.LogDiagnostics)
                Log($"Frame enqueued: {bytes.Length} bytes  type={ct2}");
        }
        finally
        {
            _hmiFetchLock.Release();
        }
    }

    // ── HMI WebSocket protocol ───────────────────────────────────────────────

    private async Task<JsonElement> SendAsync(
        string method, string path, object? body, CancellationToken ct, int? timeoutMs = null)
    {
        var id  = Interlocked.Increment(ref _reqId);
        var tcs = new TaskCompletionSource<JsonElement>(TaskCreationOptions.RunContinuationsAsynchronously);

        lock (_pendingSync) _pending[id] = tcs;

        // Cognex In-Sight HMI expects the operation verb in "$type" (not "method").
        // Verified against firmware 6.05.00 / HMI protocol 2.5: an envelope keyed
        // "method" is echoed back with body "Failure", error -2147483648.
        var msg = new Dictionary<string, object?>
        {
            ["$type"] = method,
            ["id"]    = id,
            ["path"]  = path,
            ["body"]  = body,
        };
        var json = JsonSerializer.Serialize(msg);
        var bytes = Encoding.UTF8.GetBytes(json);

        await _hmiSendLock.WaitAsync(ct);
        try { await _ws!.SendAsync(bytes, WebSocketMessageType.Text, endOfMessage: true, ct); }
        finally { _hmiSendLock.Release(); }

        var deadline = timeoutMs ?? _opts.ResponseTimeoutMs;
        using var timeoutCts = new CancellationTokenSource(deadline);
        using var linked     = CancellationTokenSource.CreateLinkedTokenSource(ct, timeoutCts.Token);

        JsonElement resp;
        try { resp = await tcs.Task.WaitAsync(linked.Token); }
        catch (OperationCanceledException) when (timeoutCts.IsCancellationRequested)
        {
            lock (_pendingSync) _pending.Remove(id);
            throw new TimeoutException($"HMI request timed out after {deadline} ms: {method} {path}");
        }

        // The HMI signals failures with an "error" property; "body" carries the reason
        // (e.g. "Member not found", "Invalid parameter"). Surface it instead of letting
        // a bogus body (like "Failure") flow downstream as if it were a valid result.
        if (resp.TryGetProperty("error", out var errEl))
        {
            var reason = resp.TryGetProperty("body", out var eb) && eb.ValueKind == JsonValueKind.String
                ? eb.GetString()
                : "unknown";
            throw new InvalidOperationException(
                $"Cognex HMI request failed: {method} {path} → \"{reason}\" (error {errEl.GetRawText()})");
        }

        return resp;
    }

    private async Task ReceiveLoopAsync(CancellationToken ct)
    {
        var buf = new byte[64 * 1024];
        var sb  = new StringBuilder();

        while (!ct.IsCancellationRequested && _ws?.State == WebSocketState.Open)
        {
            try
            {
                sb.Clear();
                WebSocketReceiveResult result;
                do
                {
                    result = await _ws.ReceiveAsync(buf, ct);
                    if (result.MessageType == WebSocketMessageType.Close) return;
                    sb.Append(Encoding.UTF8.GetString(buf, 0, result.Count));
                }
                while (!result.EndOfMessage);

                var text = sb.ToString();
                if (string.IsNullOrWhiteSpace(text)) continue;

                using var doc = JsonDocument.Parse(text);
                var root = doc.RootElement;

                if (root.TryGetProperty("id", out var idEl) && idEl.TryGetInt32(out var id))
                {
                    TaskCompletionSource<JsonElement>? tcs;
                    lock (_pendingSync) _pending.TryGetValue(id, out tcs);
                    if (tcs is not null)
                    {
                        tcs.TrySetResult(root.Clone());
                        lock (_pendingSync) _pending.Remove(id);
                    }
                }

                // resultChanged event → fetch frame
                if (root.TryGetProperty("event", out var evEl) &&
                    evEl.GetString()?.Contains("resultChanged", StringComparison.OrdinalIgnoreCase) == true)
                {
                    _ = Task.Run(() => FetchAndEnqueueFrameAsync(ct), ct);
                }
            }
            catch (OperationCanceledException) { break; }
            catch (WebSocketException) { break; }
            catch (Exception ex) { Log($"[WARN] receive: {ex.Message}"); }
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    private async Task SafeDisconnectAsync()
    {
        _hmiCts?.Cancel();

        if (_ws is { State: WebSocketState.Open })
        {
            try { await _ws.CloseAsync(WebSocketCloseStatus.NormalClosure, "bye", CancellationToken.None); }
            catch { /* best-effort */ }
        }

        _ws?.Dispose();
        _http?.Dispose();
        _ws   = null;
        _http = null;

        if (_receiveTask is not null)
        {
            try { await _receiveTask.WaitAsync(TimeSpan.FromSeconds(2)); }
            catch { /* ignore */ }
            _receiveTask = null;
        }

        _hmiCts?.Dispose();
        _hmiCts = null;
    }

    private object[] BuildLoginPayload()
    {
        // Cognex HMI login body is a 2-element array of Base64-encoded [username, password].
        // Verified live: body [{username,password}] is rejected with "Invalid parameter";
        // ["YWRtaW4=",""] (Base64 "admin","") returns access level "full".
        var user = Convert.ToBase64String(Encoding.UTF8.GetBytes(_opts.HmiUsername));
        var pass = Convert.ToBase64String(Encoding.UTF8.GetBytes(_opts.HmiPassword));
        return [user, pass];
    }

    private void Log(string msg)
    {
        if (_opts.LogDiagnostics)
            Console.WriteLine($"[CognexCamera:{_opts.CameraId}] {msg}");
    }
}
