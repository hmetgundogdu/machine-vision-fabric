export interface Contributor {
  name: string;
  handle: string;
  url: string;
  avatar: string;
  role: string;
  commits?: number;
}

/** Static list — add yourself with a PR. */
export const contributors: Contributor[] = [
  {
    name: "Ahmet Gündoğdu",
    handle: "hmetgundogdu",
    url: "https://github.com/hmetgundogdu",
    avatar: "https://github.com/hmetgundogdu.png?size=160",
    role: "Creator · maintainer",
  },
];

export const repoUrl = "https://github.com/hmetgundogdu/machine-vision-fabric";
