using System.Text.Json.Nodes;

namespace Mvf.Abstractions;

/// <summary>
/// A node that can take a new config <b>while it is running</b>, instead of being closed and reopened.
///
/// <para>The distinction this interface draws is the one the live-tunable design already rests on: what
/// can be tuned live is decided by <b>when the value is consumed</b>. A threshold read per frame is
/// tuning, and a node that reads it per frame can simply be handed a new one. Which camera to open is
/// consumed at activation, and "changing" it means closing and reopening a device — reconfiguration,
/// which stays a re-activation.</para>
///
/// <para>Without this, every live edit is a re-activation, because a module reads its config only when it
/// opens. That is correct but expensive in exactly the case where live editing is most wanted: an
/// out-of-process worker holding a loaded model pays a full process spawn and model warmup — tens of
/// seconds on a panel PC — to change a number it re-reads on every frame anyway, and the line drops
/// frames for the length of it.</para>
///
/// <para>Implementations report <see cref="CanReconfigure"/> honestly and the caller falls back to
/// re-activation when it is false, so a node that cannot do this is never worse off than before.</para>
/// </summary>
public interface IReconfigurable
{
    /// <summary>
    /// Whether a live config change is possible right now. False is a normal answer, not an error: an
    /// out-of-process worker reports what its module advertised in the handshake, and a module that
    /// never declared the capability must not be sent a message it will not answer.
    /// </summary>
    bool CanReconfigure { get; }

    /// <summary>
    /// Applies <paramref name="config"/> to the running node. Returns false when it could not be applied
    /// live and the caller should re-activate instead. Throwing means the attempt failed outright — the
    /// caller treats that the same way, by re-activating, so a broken live path can never leave a node
    /// running with a config nobody thinks it has.
    /// </summary>
    Task<bool> TryReconfigureAsync(JsonNode? config, CancellationToken cancellationToken);
}
