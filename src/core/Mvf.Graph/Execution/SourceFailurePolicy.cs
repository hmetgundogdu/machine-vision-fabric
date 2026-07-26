using System.Text.Json.Nodes;

namespace Mvf.Graph.Execution;

/// <summary>What a run does when a <b>source</b> node throws mid-stream (a camera timing out, a connection dropping).</summary>
public enum SourceFailureMode
{
    /// <summary>End the run with the source's error. The honest default: a faulted source is not a clean, empty success.</summary>
    Fail,

    /// <summary>Reconnect the source and retry the read, backing off, up to <see cref="SourceFailurePolicy.MaxRetries"/>; then fail.</summary>
    Retry,

    /// <summary>Reconnect and retry forever (capped backoff). The run rides out an outage instead of dying — for a 24/7 edge device.</summary>
    Reconnect
}

/// <summary>
/// How the runtime handles a source node that fails while producing frames.
///
/// <para>The failure policy is a <b>pipeline/runtime</b> concern, not a module one: a module's job is to throw
/// and say what broke, but whether that ends the whole run is an operational choice that differs per
/// deployment (a CI check wants <see cref="SourceFailureMode.Fail"/>; a panel PC on a line wants
/// <see cref="SourceFailureMode.Reconnect"/>). So it lives here, settable per source (the node's
/// <c>onError</c> config) with a run-level default, and applied uniformly by a decorator around the source
/// runner — the executors are unaware of it.</para>
///
/// <para>The default is <see cref="SourceFailureMode.Fail"/> so the "a faulted source is not a clean success"
/// guarantee holds unless a deployment opts into resilience.</para>
/// </summary>
public sealed record SourceFailurePolicy
{
    /// <summary>What to do on a source read failure.</summary>
    public SourceFailureMode Mode { get; init; } = SourceFailureMode.Fail;

    /// <summary>For <see cref="SourceFailureMode.Retry"/>: how many reconnect attempts before the run fails.</summary>
    public int MaxRetries { get; init; } = 4;

    /// <summary>First backoff, doubled each attempt up to <see cref="MaxBackoffMs"/>.</summary>
    public int BaseBackoffMs { get; init; } = 500;

    /// <summary>Upper bound on a single backoff wait.</summary>
    public int MaxBackoffMs { get; init; } = 5_000;

    /// <summary>The honest fail-fast default.</summary>
    public static SourceFailurePolicy Fail { get; } = new();

    /// <summary>True when the policy retries at all (i.e. the source runner needs wrapping).</summary>
    public bool WillRetry => Mode is SourceFailureMode.Retry or SourceFailureMode.Reconnect;

    /// <summary>Whether attempt <paramref name="attempt"/> (1-based) is allowed. Unbounded for reconnect.</summary>
    public bool AllowsAttempt(int attempt) => Mode switch
    {
        SourceFailureMode.Reconnect => true,
        SourceFailureMode.Retry => attempt <= MaxRetries,
        _ => false
    };

    /// <summary>The backoff before reconnect attempt <paramref name="attempt"/> (1-based): exponential, capped.</summary>
    public TimeSpan BackoffFor(int attempt)
    {
        var shift = Math.Clamp(attempt - 1, 0, 30);
        var ms    = Math.Min((long)Math.Max(0, BaseBackoffMs) << shift, Math.Max(0, MaxBackoffMs));
        return TimeSpan.FromMilliseconds(ms);
    }

    /// <summary>
    /// Reads a node's <c>onError</c> config, falling back to <paramref name="fallback"/>. Accepts a string
    /// shorthand (<c>"fail"</c>/<c>"retry"</c>/<c>"reconnect"</c>) or an object
    /// <c>{ mode, maxRetries, backoffMs, maxBackoffMs }</c>. An unrecognised value keeps the fallback.
    /// </summary>
    public static SourceFailurePolicy FromConfig(JsonObject? config, SourceFailurePolicy fallback)
    {
        if (config is null || !config.TryGetPropertyValue("onError", out var node) || node is null)
        {
            return fallback;
        }

        if (node is JsonValue s && s.TryGetValue(out string? text) && text is not null)
        {
            return TryParseMode(text, out var mode) ? fallback with { Mode = mode } : fallback;
        }

        if (node is JsonObject obj)
        {
            var mode = obj.TryGetPropertyValue("mode", out var mn)
                       && mn is JsonValue mv && mv.TryGetValue(out string? ms) && TryParseMode(ms, out var pm)
                ? pm
                : fallback.Mode;

            var policy = fallback with { Mode = mode };
            if (TryReadInt(obj, "maxRetries",   out var mr)) policy = policy with { MaxRetries   = mr };
            if (TryReadInt(obj, "backoffMs",    out var bo)) policy = policy with { BaseBackoffMs = bo };
            if (TryReadInt(obj, "maxBackoffMs", out var mb)) policy = policy with { MaxBackoffMs  = mb };
            return policy;
        }

        return fallback;
    }

    /// <summary>Parses a mode token; <c>"forever"</c> is an alias for <see cref="SourceFailureMode.Reconnect"/>.</summary>
    public static bool TryParseMode(string? text, out SourceFailureMode mode)
    {
        switch (text?.Trim().ToLowerInvariant())
        {
            case "fail":                 mode = SourceFailureMode.Fail;      return true;
            case "retry":                mode = SourceFailureMode.Retry;     return true;
            case "reconnect": case "forever": mode = SourceFailureMode.Reconnect; return true;
            default:                     mode = SourceFailureMode.Fail;      return false;
        }
    }

    private static bool TryReadInt(JsonObject obj, string key, out int value)
    {
        value = 0;
        return obj.TryGetPropertyValue(key, out var n) && n is JsonValue jv && jv.TryGetValue(out value);
    }
}
