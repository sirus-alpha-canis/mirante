// Threads as anything inside a task's Detail may ask for them: start a thread from a line of
// this conversation, or open one (with its composer focused, to continue it). Its own module
// so feed lines need nothing else of the Detail. Null outside a Detail (Home's cards).
import { createContext, useContext } from "react";
import type { FeedItem } from "../store/state";

export interface ThreadApi {
  /** "New thread from this" on a person's message or an agent's final answer. */
  newThreadFrom(item: FeedItem): void;
  /** Opens a thread's Detail; `composer`: its composer gets the keyboard (Continue). */
  open(id: string, composer?: boolean): void;
}

export const ThreadContext = createContext<ThreadApi | null>(null);

export function useThreads(): ThreadApi | null {
  return useContext(ThreadContext);
}
