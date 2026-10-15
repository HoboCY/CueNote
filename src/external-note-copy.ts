/** Creating an independent copy must not flush pending edits to the original. */
export async function copyExternalDraft<Content, Receipt>(options: {
  pauseSaving: () => Promise<void>;
  settleEditors: () => Promise<void>;
  snapshot: () => { sessionId: string; content: Content };
  publish: (snapshot: { sessionId: string; content: Content }) => Promise<Receipt>;
}): Promise<Receipt> {
  await options.pauseSaving();
  await options.settleEditors();
  return options.publish(options.snapshot());
}
