using MachineVisionFabric.Contracts.Runtime;

namespace MachineVisionFabric.Core.Abstractions;

public interface IHeadlessRuntimeBootstrapper
{
    Task<HeadlessBootstrapReport> BootstrapAsync(CancellationToken cancellationToken);
}
