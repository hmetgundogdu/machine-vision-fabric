using Mvf.Abstractions;

namespace Mvf.Engine.Values;

/// <summary>
/// Tries each resolver in order and takes the first real answer. Order is the policy: a machine-supplied
/// value (an environment variable) should win over asking a person, so an unattended deployment never
/// depends on someone being there.
/// </summary>
public sealed class ChainedValueResolver(params IValueResolver[] resolvers) : IValueResolver
{
    public bool CanResolve => resolvers.Any(r => r.CanResolve);

    public async Task<ValueResolution> ResolveAsync(ValueRequest request, CancellationToken cancellationToken)
    {
        foreach (var resolver in resolvers)
        {
            if (!resolver.CanResolve)
            {
                continue;
            }

            var resolution = await resolver.ResolveAsync(request, cancellationToken);

            // A failure is reported rather than skipped past: a resolver that tried and could not produce
            // a usable value is a different thing from one that had nothing to offer.
            if (resolution.Resolved || resolution.Error is { Length: > 0 })
            {
                return resolution;
            }
        }

        return ValueResolution.Unresolved;
    }
}
