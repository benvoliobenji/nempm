import type { AppError } from "@/bindings/AppError";
import type { ErrorKind } from "@/bindings/ErrorKind";

/** Normalises anything a command or the browser can throw into AppError. */
export function toAppError(err: unknown): AppError {
  if (err && typeof err === "object" && "kind" in err && "message" in err) {
    const e = err as { kind: unknown; message: unknown };
    return { kind: String(e.kind) as ErrorKind, message: String(e.message) };
  }
  if (err instanceof Error) return { kind: "io", message: err.message };
  if (typeof err === "string") return { kind: "io", message: err };
  return { kind: "io", message: "Something went wrong." };
}

export function errorMessage(err: unknown): string {
  return toAppError(err).message;
}

export function isKind(err: unknown, kind: ErrorKind): boolean {
  return toAppError(err).kind === kind;
}
