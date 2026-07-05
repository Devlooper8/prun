export const $ = <T extends Element>(s: string) => document.querySelector<T>(s)!;
