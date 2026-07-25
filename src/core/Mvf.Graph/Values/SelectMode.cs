namespace Mvf.Graph.Values;

/// <summary>
/// What a <c>select</c> emits: a single element (<see cref="One"/>) or the narrowed collection
/// (<see cref="Many"/>). This is the only thing that changes its output port type, which is why it is a
/// mode rather than two primitives.
/// </summary>
public enum SelectMode
{
    One,
    Many
}

public static class SelectModes
{
    public const string Supported = "one, many";

    public static bool TryParse(string? value, out SelectMode mode)
    {
        switch (value?.Trim().ToLowerInvariant())
        {
            case "one": mode = SelectMode.One; return true;
            case "many": mode = SelectMode.Many; return true;
            default: mode = SelectMode.One; return false;
        }
    }

    public static string ToToken(SelectMode mode) => mode == SelectMode.Many ? "many" : "one";
}
