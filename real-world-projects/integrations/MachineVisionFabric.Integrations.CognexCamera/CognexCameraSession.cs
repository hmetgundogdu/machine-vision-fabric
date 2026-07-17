using System.Net.Http.Headers;
using System.Net.WebSockets;
using System.Text;
using System.Text.Json;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Integrations.CognexCamera;

public sealed class CognexCameraSession : BackgroundFrameSourceSession
{
    private readonly SemaphoreSlim hmiSendLock = new(1, 1);
    private readonly SemaphoreSlim hmiFetchLock = new(1, 1);
    private readonly Dictionary<int, TaskCompletionSource<JsonElement>> pendingRequests = [];
    private readonly object pendingSync = new();
    private readonly TaskCompletionSource completionSource = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly CognexCameraOptions options;

    private ClientWebSocket? hmiWebSocket;
    private HttpClient? hmiHttpClient;
    private CancellationTokenSource? hmiCancellationSource;
    private Task? hmiReceiveTask;
    private string? hmiSessionUserPath;
    private int hmiRequestId = 100;
    private int producedFrameCount;

    public CognexCameraSession(CognexCameraOptions options)
        : base(
            declaredCameraCount: 1,
            estimatedFrameCount: ResolveEstimatedFrameCount(options),
            boundedCapacity: options.BoundedCapacity)
    {
        this.options = options;
        StartBackgroundProducer(ProduceFramesAsync);
    }

    public CognexCameraOptions Options => options;

    private async Task ProduceFramesAsync(CancellationToken cancellationToken)
    {
        try
        {
            if (options.StartupDelayMs > 0)
            {
                Log($"Startup delay: {options.StartupDelayMs} ms");
                await Task.Delay(options.StartupDelayMs, cancellationToken);
            }

            await ConnectViaHmiAsync(cancellationToken);

            if (IsManualTriggerLoopMode())
            {
                await RunManualTriggerLoopAsync(cancellationToken);
                return;
            }

            await RunPassiveListenLoopAsync(cancellationToken);
        }
        finally
        {
            await SafeDisconnectHmiAsync();
            hmiSendLock.Dispose();
            hmiFetchLock.Dispose();
        }
    }

    private async Task ConnectViaHmiAsync(CancellationToken cancellationToken)
    {
        await SafeDisconnectHmiAsync();

        var socket = new ClientWebSocket();
        var webSocketScheme = options.HmiUseTls ? "wss" : "ws";
        var webSocketPath = options.HmiWebSocketPath.StartsWith("/", StringComparison.Ordinal)
            ? options.HmiWebSocketPath
            : "/" + options.HmiWebSocketPath;
        var webSocketUri = new Uri($"{webSocketScheme}://{options.IpAddress}:{options.HmiPort}{webSocketPath}");
        Log($"Connecting HMI WebSocket: {webSocketUri}");

        hmiCancellationSource = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        await socket.ConnectAsync(webSocketUri, cancellationToken);
        hmiWebSocket = socket;
        Log("HMI WebSocket connected");

        var httpScheme = options.HmiUseTls ? "https" : "http";
        hmiHttpClient = new HttpClient
        {
            BaseAddress = new Uri($"{httpScheme}://{options.IpAddress}:{options.HmiPort}"),
            Timeout = TimeSpan.FromMilliseconds(Math.Max(1000, options.ResponseTimeoutMs))
        };

        hmiReceiveTask = Task.Run(() => HmiReceiveLoopAsync(hmiCancellationSource.Token), hmiCancellationSource.Token);

        await SendHmiRequestAsync("get", "system/settings", null, cancellationToken);
        await SendHmiRequestAsync("get", "system/info", null, cancellationToken);
        await SendHmiRequestAsync("get", "system/job", null, cancellationToken);
        await OpenSessionAsync(cancellationToken);
    }

    private async Task OpenSessionAsync(CancellationToken cancellationToken)
    {
        var openSessionBody = new object[]
        {
            new Dictionary<string, object?>()
            {
                ["$type"] = "HmiSessionInfo",
                ["enableQueuedResults"] = true,
                ["cellNames"] = null,
                ["includeCustomView"] = true,
                ["includeEasyView"] = true
            }
        };

        var openSessionResponse = await SendHmiRequestAsync("post", "system/openSession", openSessionBody, cancellationToken);
        var sessionPath = openSessionResponse.TryGetProperty("body", out var bodyElement) && bodyElement.ValueKind == JsonValueKind.String
            ? bodyElement.GetString()
            : null;

        if (string.IsNullOrWhiteSpace(sessionPath))
        {
            throw new InvalidOperationException("Cognex HMI openSession did not return a user session path.");
        }

        if (sessionPath.Contains("Access denied", StringComparison.OrdinalIgnoreCase))
        {
            throw new InvalidOperationException("Cognex HMI openSession returned access denied.");
        }

        hmiSessionUserPath = sessionPath;
        Log($"HMI session opened: {hmiSessionUserPath}");

        await SendHmiRequestAsync("post", $"{sessionPath}/login", BuildHmiLoginPayload(), cancellationToken);
        Log("HMI login completed");
        await SendHmiRequestAsync("listen", $"{sessionPath}/resultChanged", null, cancellationToken);
        await SendHmiRequestAsync("listen", "system/stateChanged", null, cancellationToken);
        Log("HMI listeners registered: resultChanged, system/stateChanged");
    }

    private async Task ReopenSessionAsync(CancellationToken cancellationToken)
    {
        if (!string.IsNullOrWhiteSpace(hmiSessionUserPath))
        {
            try
            {
                await SendHmiRequestAsync("post", $"{hmiSessionUserPath}/dispose", Array.Empty<object>(), cancellationToken, 700);
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
            }
        }

        await OpenSessionAsync(cancellationToken);
        Log($"HMI session reopened: {hmiSessionUserPath}");
    }

    private async Task RunManualTriggerLoopAsync(CancellationToken cancellationToken)
    {
        while (!cancellationToken.IsCancellationRequested)
        {
            if (options.MaxFrames is int maxFrames && Volatile.Read(ref producedFrameCount) >= maxFrames)
            {
                completionSource.TrySetResult();
                break;
            }

            await TriggerViaHmiAsync(cancellationToken);

            if (options.MaxFrames is int frameLimit && Volatile.Read(ref producedFrameCount) >= frameLimit)
            {
                completionSource.TrySetResult();
                break;
            }

            if (options.ManualTriggerIntervalMs > 0)
            {
                await Task.Delay(options.ManualTriggerIntervalMs, cancellationToken);
            }
        }

        await WaitForCompletionAsync(cancellationToken);
    }

    private async Task RunPassiveListenLoopAsync(CancellationToken cancellationToken)
    {
        Log($"Passive listen mode started. Ready interval: {options.HmiReadyIntervalMs} ms");

        if (!string.IsNullOrWhiteSpace(hmiSessionUserPath))
        {
            await TrySendReadyAsync(cancellationToken);
        }

        while (!cancellationToken.IsCancellationRequested)
        {
            if (options.MaxFrames is int maxFrames && Volatile.Read(ref producedFrameCount) >= maxFrames)
            {
                completionSource.TrySetResult();
                break;
            }

            if (options.HmiReadyIntervalMs > 0 && !string.IsNullOrWhiteSpace(hmiSessionUserPath))
            {
                await TrySendReadyAsync(cancellationToken);
            }

            await Task.Delay(Math.Max(250, options.HmiReadyIntervalMs), cancellationToken);
        }

        await WaitForCompletionAsync(cancellationToken);
    }

    private async Task TriggerViaHmiAsync(CancellationToken cancellationToken)
    {
        if (string.IsNullOrWhiteSpace(hmiSessionUserPath))
        {
            throw new InvalidOperationException("Cognex HMI session is not initialized.");
        }

        if (options.ReopenSessionBeforeManualTrigger)
        {
            await ReopenSessionAsync(cancellationToken);
        }

        Exception? lastError = null;
        var retryCount = Math.Max(1, options.ManualTriggerRetryCount);

        for (var attempt = 1; attempt <= retryCount; attempt++)
        {
            try
            {
                try
                {
                    await SendHmiRequestAsync("post", $"{hmiSessionUserPath}/manualTrigger", new object[] { true }, cancellationToken, 600);
                }
                catch (Exception ex) when (ex is not OperationCanceledException)
                {
                }

                await SendHmiRequestAsync("post", $"{hmiSessionUserPath}/ready", Array.Empty<object>(), cancellationToken, 900);
                return;
            }
            catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
            {
                throw;
            }
            catch (Exception ex)
            {
                lastError = ex;
                await Task.Delay(60, cancellationToken);
            }
        }

        throw new InvalidOperationException("Cognex HMI trigger failed after retries.", lastError);
    }

    private async Task HmiReceiveLoopAsync(CancellationToken cancellationToken)
    {
        var webSocket = hmiWebSocket;
        if (webSocket is null)
        {
            return;
        }

        var buffer = new byte[64 * 1024];
        using var memoryStream = new MemoryStream();

        while (!cancellationToken.IsCancellationRequested && webSocket.State == WebSocketState.Open)
        {
            memoryStream.SetLength(0);

            WebSocketReceiveResult? receiveResult;
            do
            {
                receiveResult = await webSocket.ReceiveAsync(new ArraySegment<byte>(buffer), cancellationToken);
                if (receiveResult.MessageType == WebSocketMessageType.Close)
                {
                    return;
                }

                memoryStream.Write(buffer, 0, receiveResult.Count);
            }
            while (!receiveResult.EndOfMessage);

            var payload = Encoding.UTF8.GetString(memoryStream.GetBuffer(), 0, (int)memoryStream.Length);
            if (string.IsNullOrWhiteSpace(payload))
            {
                continue;
            }

            JsonElement root;
            try
            {
                using var document = JsonDocument.Parse(payload);
                root = document.RootElement.Clone();
            }
            catch (JsonException)
            {
                continue;
            }

            if (!root.TryGetProperty("$type", out var typeElement))
            {
                continue;
            }

            var messageType = typeElement.GetString();
            if (string.Equals(messageType, "resp", StringComparison.OrdinalIgnoreCase))
            {
                CompletePendingResponse(root);
                continue;
            }

            if (string.Equals(messageType, "event", StringComparison.OrdinalIgnoreCase))
            {
                var eventPath = root.TryGetProperty("path", out var pathElement) ? pathElement.GetString() : "<unknown>";
                Log($"HMI event received: {eventPath}");
                _ = Task.Run(() => HandleHmiEventAsync(root, cancellationToken), cancellationToken);
            }
        }
    }

    private void CompletePendingResponse(JsonElement root)
    {
        if (!root.TryGetProperty("id", out var idElement) || !idElement.TryGetInt32(out var id))
        {
            return;
        }

        TaskCompletionSource<JsonElement>? completion = null;
        lock (pendingSync)
        {
            if (pendingRequests.TryGetValue(id, out var existing))
            {
                completion = existing;
                pendingRequests.Remove(id);
            }
        }

        completion?.TrySetResult(root);
    }

    private async Task HandleHmiEventAsync(JsonElement root, CancellationToken cancellationToken)
    {
        if (!TryExtractHmiImageUrl(root, out var imageUrl))
        {
            Log("HMI event did not contain an image URL");
            return;
        }

        Log($"HMI image URL detected: {imageUrl}");
        await FetchAndPublishHmiImageAsync(imageUrl!, cancellationToken);
    }

    private async Task FetchAndPublishHmiImageAsync(string imageUrl, CancellationToken cancellationToken)
    {
        if (hmiHttpClient is null)
        {
            return;
        }

        await hmiFetchLock.WaitAsync(cancellationToken);
        try
        {
            var imageUri = BuildHmiImageUri(imageUrl);
            using var response = await hmiHttpClient.GetAsync(imageUri, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
            response.EnsureSuccessStatusCode();

            var bytes = await response.Content.ReadAsByteArrayAsync(cancellationToken);
            if (bytes.Length == 0)
            {
                return;
            }

            var sequenceNumber = Interlocked.Increment(ref producedFrameCount);
            if (options.MaxFrames is int maxFrames && sequenceNumber > maxFrames)
            {
                completionSource.TrySetResult();
                return;
            }

            var contentType = ResolveContentType(response.Content.Headers.ContentType, imageUrl);
            var extension = ResolveExtension(contentType, imageUrl);
            var fileName = $"{options.CameraId}-seq{sequenceNumber:0000}{extension}";
            var sourcePath = BuildAbsoluteImageUrl(imageUri);
            var frame = FrameEnvelopeFactory.FromBytes(
                options.CameraId,
                sequenceNumber,
                fileName,
                bytes,
                contentType,
                DateTime.UtcNow,
                sourcePath);

            await PublishAsync(frame, cancellationToken);
            Log($"Frame published: seq={sequenceNumber}; bytes={bytes.Length}; contentType={contentType}; source={sourcePath}");

            if (options.MaxFrames is int limit && sequenceNumber >= limit)
            {
                completionSource.TrySetResult();
            }
        }
        finally
        {
            hmiFetchLock.Release();
        }
    }

    private Uri BuildHmiImageUri(string imageUrl)
    {
        var relativePath = imageUrl.StartsWith("/", StringComparison.Ordinal)
            ? imageUrl
            : "/" + imageUrl;
        var extraQuery = string.IsNullOrWhiteSpace(options.HmiImageQuery)
            ? string.Empty
            : options.HmiImageQuery.TrimStart('?');
        var cacheBust = $"_ts={DateTime.UtcNow.Ticks}";
        var mergedQuery = string.IsNullOrWhiteSpace(extraQuery)
            ? cacheBust
            : $"{extraQuery}&{cacheBust}";
        var separator = relativePath.Contains('?', StringComparison.Ordinal) ? "&" : "?";

        return new Uri(relativePath + separator + mergedQuery, UriKind.Relative);
    }

    private string BuildAbsoluteImageUrl(Uri imageUri)
    {
        if (hmiHttpClient?.BaseAddress is null)
        {
            return imageUri.ToString();
        }

        return new Uri(hmiHttpClient.BaseAddress, imageUri).ToString();
    }

    private async Task<JsonElement> SendHmiRequestAsync(
        string type,
        string path,
        object? body,
        CancellationToken cancellationToken,
        int? timeoutMs = null)
    {
        var webSocket = hmiWebSocket;
        if (webSocket is null || webSocket.State != WebSocketState.Open)
        {
            throw new InvalidOperationException("Cognex HMI websocket is not connected.");
        }

        var requestId = Interlocked.Increment(ref hmiRequestId);
        var request = new Dictionary<string, object?>
        {
            ["$type"] = type,
            ["id"] = requestId,
            ["path"] = path
        };

        if (body is not null)
        {
            request["body"] = body;
        }

        var payload = Encoding.UTF8.GetBytes(JsonSerializer.Serialize(request));
        var pendingResponse = new TaskCompletionSource<JsonElement>(TaskCreationOptions.RunContinuationsAsynchronously);

        lock (pendingSync)
        {
            pendingRequests[requestId] = pendingResponse;
        }

        await hmiSendLock.WaitAsync(cancellationToken);
        try
        {
            await webSocket.SendAsync(payload, WebSocketMessageType.Text, true, cancellationToken);
        }
        finally
        {
            hmiSendLock.Release();
        }

        var effectiveTimeoutMs = timeoutMs ?? Math.Max(1000, options.ResponseTimeoutMs);
        var completedTask = await Task.WhenAny(pendingResponse.Task, Task.Delay(effectiveTimeoutMs, cancellationToken));
        if (completedTask != pendingResponse.Task)
        {
            lock (pendingSync)
            {
                pendingRequests.Remove(requestId);
            }

            Log($"HMI request timeout: type={type}; path={path}");
            throw new InvalidOperationException($"Cognex HMI request timeout: {type} {path}");
        }

        var response = await pendingResponse.Task;
        if (response.TryGetProperty("error", out var errorElement)
            && errorElement.ValueKind == JsonValueKind.Number
            && errorElement.TryGetInt32(out var errorCode)
            && errorCode != 0)
        {
            var bodyText = response.TryGetProperty("body", out var errorBody)
                ? errorBody.ToString()
                : "<empty>";
            throw new InvalidOperationException(
                $"Cognex HMI request failed ({type} {path}), code={errorCode}, body={bodyText}");
        }

        return response;
    }

    private async Task WaitForCompletionAsync(CancellationToken cancellationToken)
    {
        if (options.MaxFrames is int)
        {
            await completionSource.Task.WaitAsync(cancellationToken);
            return;
        }

        await Task.Delay(Timeout.Infinite, cancellationToken);
    }

    private async Task SafeDisconnectHmiAsync()
    {
        try
        {
            hmiCancellationSource?.Cancel();
        }
        catch
        {
        }

        lock (pendingSync)
        {
            foreach (var pending in pendingRequests.Values)
            {
                pending.TrySetCanceled();
            }

            pendingRequests.Clear();
        }

        if (hmiReceiveTask is not null)
        {
            try
            {
                await hmiReceiveTask;
            }
            catch
            {
            }
            finally
            {
                hmiReceiveTask = null;
            }
        }

        if (hmiWebSocket is not null)
        {
            try
            {
                if (hmiWebSocket.State == WebSocketState.Open)
                {
                    await hmiWebSocket.CloseAsync(WebSocketCloseStatus.NormalClosure, "shutdown", CancellationToken.None);
                }
            }
            catch
            {
            }

            hmiWebSocket.Dispose();
            hmiWebSocket = null;
        }

        hmiHttpClient?.Dispose();
        hmiHttpClient = null;

        hmiCancellationSource?.Dispose();
        hmiCancellationSource = null;
        hmiSessionUserPath = null;
    }

    private object[] BuildHmiLoginPayload()
    {
        var username = Convert.ToBase64String(Encoding.UTF8.GetBytes(options.Username));
        var password = Convert.ToBase64String(Encoding.UTF8.GetBytes(options.Password));
        return [username, password];
    }

    private bool TryExtractHmiImageUrl(JsonElement node, out string? imageUrl)
    {
        imageUrl = null;

        if (node.ValueKind == JsonValueKind.Object && node.TryGetProperty("body", out var bodyNode))
        {
            return TryExtractHmiImageUrl(bodyNode, out imageUrl);
        }

        if (node.ValueKind != JsonValueKind.Object)
        {
            return false;
        }

        if (!node.TryGetProperty("acqImageView", out var imageViewNode) || imageViewNode.ValueKind != JsonValueKind.Object)
        {
            return false;
        }

        if (!imageViewNode.TryGetProperty("layers", out var layersNode) || layersNode.ValueKind != JsonValueKind.Array)
        {
            return false;
        }

        foreach (var layer in layersNode.EnumerateArray())
        {
            if (!layer.TryGetProperty("$type", out var typeNode))
            {
                continue;
            }

            if (!string.Equals(typeNode.GetString(), "ImageLayer", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            if (!layer.TryGetProperty("url", out var urlNode) || urlNode.ValueKind != JsonValueKind.String)
            {
                continue;
            }

            var url = urlNode.GetString();
            if (string.IsNullOrWhiteSpace(url))
            {
                continue;
            }

            imageUrl = url;
            return true;
        }

        return false;
    }

    private static string ResolveContentType(MediaTypeHeaderValue? contentTypeHeader, string imageUrl)
    {
        var mediaType = contentTypeHeader?.MediaType;
        if (!string.IsNullOrWhiteSpace(mediaType))
        {
            return mediaType;
        }

        return ResolveExtension(null, imageUrl) switch
        {
            ".jpg" or ".jpeg" => "image/jpeg",
            ".png" => "image/png",
            ".bmp" => "image/bmp",
            ".tif" or ".tiff" => "image/tiff",
            _ => "application/octet-stream"
        };
    }

    private static string ResolveExtension(string? contentType, string imageUrl)
    {
        if (!string.IsNullOrWhiteSpace(contentType))
        {
            return contentType.ToLowerInvariant() switch
            {
                "image/jpeg" => ".jpg",
                "image/png" => ".png",
                "image/bmp" => ".bmp",
                "image/tiff" => ".tif",
                _ => ResolveExtensionFromUrl(imageUrl)
            };
        }

        return ResolveExtensionFromUrl(imageUrl);
    }

    private static string ResolveExtensionFromUrl(string imageUrl)
    {
        var cleanUrl = imageUrl.Split('?', '#')[0];
        var extension = Path.GetExtension(cleanUrl);
        return string.IsNullOrWhiteSpace(extension) ? ".bin" : extension.ToLowerInvariant();
    }

    private static int? ResolveEstimatedFrameCount(CognexCameraOptions options)
    {
        return options.MaxFrames is > 0 ? options.MaxFrames : null;
    }

    private bool IsManualTriggerLoopMode()
    {
        return string.Equals(options.AcquisitionMode, "manual-trigger-loop", StringComparison.OrdinalIgnoreCase);
    }

    private async Task TrySendReadyAsync(CancellationToken cancellationToken)
    {
        try
        {
            await SendHmiRequestAsync("post", $"{hmiSessionUserPath}/ready", Array.Empty<object>(), cancellationToken, 900);
            Log("HMI ready sent");
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
        {
            throw;
        }
        catch (Exception ex)
        {
            Log($"HMI ready failed: {ex.Message}");
        }
    }

    private void Log(string message)
    {
        if (!options.LogDiagnostics)
        {
            return;
        }

        Console.WriteLine($"[CognexCamera] {DateTime.Now:yyyy-MM-dd HH:mm:ss.fff} {message}");
    }
}
