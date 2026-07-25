namespace Mvf.Graph.Values;

/// <summary>Whether a control-value port carries one value or a homogeneous collection of them.</summary>
public enum ControlValueShape
{
    Single,
    List
}

/// <summary>
/// The two port data-type families that carry values rather than decisions:
/// <c>control/value:&lt;t&gt;</c> and <c>control/list:&lt;t&gt;</c>, where <c>&lt;t&gt;</c> is one of
/// <see cref="ControlValueTypes.Supported"/>.
///
/// <para>These are ordinary port types, checked by the same validator rule as every other edge — no
/// device-specific type, and no loose object passing.</para>
/// </summary>
public static class ControlPortTypes
{
    public const string ValuePrefix = "control/value:";
    public const string ListPrefix = "control/list:";

    public static string Value(ControlValueType type) => ValuePrefix + ControlValueTypes.ToToken(type);

    public static string List(ControlValueType type) => ListPrefix + ControlValueTypes.ToToken(type);

    public static string Of(ControlValueShape shape, ControlValueType type) =>
        shape == ControlValueShape.List ? List(type) : Value(type);

    /// <summary>
    /// Parses a port data type. Returns false for every other data type (<c>data/frame</c>,
    /// <c>control/boolean-gate</c>, …) and for an unknown element type, which the validator reports as
    /// <c>pipeline.node.invalid-value-type</c> rather than silently accepting.
    /// </summary>
    public static bool TryParse(string? dataType, out ControlValueShape shape, out ControlValueType type)
    {
        shape = ControlValueShape.Single;
        type = ControlValueType.Json;

        if (dataType is null)
        {
            return false;
        }

        string token;
        if (dataType.StartsWith(ValuePrefix, StringComparison.OrdinalIgnoreCase))
        {
            shape = ControlValueShape.Single;
            token = dataType[ValuePrefix.Length..];
        }
        else if (dataType.StartsWith(ListPrefix, StringComparison.OrdinalIgnoreCase))
        {
            shape = ControlValueShape.List;
            token = dataType[ListPrefix.Length..];
        }
        else
        {
            return false;
        }

        return ControlValueTypes.TryParse(token, out type);
    }

    /// <summary>True when this data type is one of the two value families, whatever its element type.</summary>
    public static bool IsControlValue(string? dataType) =>
        dataType is not null
        && (dataType.StartsWith(ValuePrefix, StringComparison.OrdinalIgnoreCase)
            || dataType.StartsWith(ListPrefix, StringComparison.OrdinalIgnoreCase));
}
