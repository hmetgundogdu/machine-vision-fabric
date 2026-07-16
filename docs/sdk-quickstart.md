# MachineVisionFabric SDK Quickstart

## Purpose

`MachineVisionFabric.Sdk` is the single integration entry point for external module authors.

If you want to add a real camera vendor such as Cognex later, start from this SDK layer instead of coding directly against the runtime.

## Main SDK Types

- `FrameSourceModuleBase<TOptions>`
- `ProductPresenceGateModuleBase<TOptions>`
- `BackgroundFrameSourceSession`
- `FrameEnvelopeFactory`
- `PackagePathResolver`
- `IntegrationModuleDescriptorBuilder`

## Recommended Starting Template

Use this example as the base for a real camera source module:

- [ResidentCameraStubIntegrationModule.cs](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\examples\integrations\MachineVisionFabric.Integrations.ResidentCameraStub\ResidentCameraStubIntegrationModule.cs)
- [ResidentCameraStubSession.cs](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\examples\integrations\MachineVisionFabric.Integrations.ResidentCameraStub\ResidentCameraStubSession.cs)
- [ResidentCameraStubOptions.cs](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\examples\integrations\MachineVisionFabric.Integrations.ResidentCameraStub\ResidentCameraStubOptions.cs)

If you want to start a project-local adapter under the same repository boundary, use:

- [New-MvfRealWorldIntegration.ps1](C:\Users\c9018243a\Desktop\Projects\machine-vision-fabric\real-world-projects\tools\New-MvfRealWorldIntegration.ps1)

## Cognex Mapping

When you bring Cognex logic from another project, the mapping should be:

1. keep the new adapter in its own project, for example `MachineVisionFabric.Integrations.CognexCamera`
2. reference `MachineVisionFabric.Sdk`
3. derive the module entry class from `FrameSourceModuleBase<TOptions>`
4. derive the live session class from `BackgroundFrameSourceSession`
5. open the Cognex camera inside the session producer
6. convert each SDK frame callback or polled image into `FrameEnvelopeFactory.FromBytes(...)`
7. publish each frame through `PublishAsync(...)`

## What You Replace

Inside the resident stub example, these are the parts you replace with vendor SDK logic:

- `ResolveFiles(...)`
- `CreateEnvelopeAsync(...)`
- `ProduceFramesAsync(...)`

The platform-facing shape stays the same.

## Minimal Runtime Contract

Your real adapter only needs to satisfy this flow:

`Vendor SDK -> BackgroundFrameSourceSession -> IFrameEnvelope -> DatasetCollector`

The runtime does not need to know anything about the vendor-specific API.

## Practical Rule

If a file needs a vendor DLL, customer station assumptions, or camera-specific setup, it should not go into `src/`.
It belongs in an external integration project built on top of `MachineVisionFabric.Sdk`.
