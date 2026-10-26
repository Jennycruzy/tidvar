declare module "qrcode" {
  export function toCanvas(canvas: HTMLCanvasElement, text: string, options: {width: number; margin: number}): Promise<void>;
}
