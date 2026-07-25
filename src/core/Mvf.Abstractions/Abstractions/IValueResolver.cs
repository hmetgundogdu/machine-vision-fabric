using System.Text.Json.Nodes;
using Mvf.Graph.Values;

namespace Mvf.Abstractions;

/// <summary>
/// Obtains a value the graph cannot compute — a terminal prompt, an environment variable, later the
/// studio. The primitive owns the semantics (what the value means, what type it has, where it is
/// stored); a resolver only knows how to ask.
///
/// <para>Same shape as <see cref="IDataPlane"/> and <see cref="IOutOfProcessModuleHost"/>: the core
/// declares the seam, implementations live at the edges. This is deliberately <b>not</b> a UI
/// description — the resolver renders <i>a type</i>, never a widget spec carried in the graph.</para>
/// </summary>
public interface IValueResolver
{
    /// <summary>
    /// Whether this resolver can ask right now. A terminal resolver says no when stdin is redirected or
    /// prompting was disabled, so an unattended run fails with an actionable message instead of hanging
    /// on a human who is not there.
    /// </summary>
    bool CanResolve { get; }

    Task<ValueResolution> ResolveAsync(ValueRequest request, CancellationToken cancellationToken);
}

/// <summary>
/// What is being asked for. <paramref name="Choices"/> is what turns a prompt into a picker: when it is
/// non-null the answer must be one of those elements, which is exactly the camera case — an unresolved
/// criterion over a discovered collection.
/// </summary>
public sealed record ValueRequest(
    string NodeId,
    ControlValueType Type,
    string? Binding = null,
    string? Prompt = null,
    JsonNode? Schema = null,
    IReadOnlyList<JsonNode?>? Choices = null,
    string? ChoiceLabelProperty = null,
    ControlValueShape Shape = ControlValueShape.Single,
    /// <summary>
    /// The node's declared default, offered as the pre-filled answer so pressing enter accepts it.
    /// It is still the last resort in the resolution order — this only means an operator who is asked
    /// can see and take it, instead of having to retype a value the pipeline already suggested.
    /// </summary>
    JsonNode? Default = null);

/// <summary>
/// <paramref name="Persist"/> says whether this answer belongs in the binding store. True for a prompt —
/// the point of asking a person is never to ask them again. False when the resolver read a source that is
/// already durable and already authoritative, an environment variable above all: the store is consulted
/// *before* any resolver, so caching an environment value would freeze the first run's configuration into
/// the machine and quietly ignore every later change to it.
/// </summary>
public sealed record ValueResolution(bool Resolved, JsonNode? Value, string? Error, bool Persist = false)
{
    /// <summary>This resolver has nothing to offer; the caller falls through to the next source.</summary>
    public static readonly ValueResolution Unresolved = new(false, null, null);

    /// <summary>An answer worth remembering — stored under the binding so later runs never ask again.</summary>
    public static ValueResolution Ok(JsonNode? value) => new(true, value, null, Persist: true);

    /// <summary>An answer read from a source that is already durable; used, not cached.</summary>
    public static ValueResolution Transient(JsonNode? value) => new(true, value, null, Persist: false);

    /// <summary>The resolver tried and could not produce a usable value; the run stops with this message.</summary>
    public static ValueResolution Failed(string error) => new(false, null, error);
}
