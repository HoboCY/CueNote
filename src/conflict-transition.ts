export interface DraftSnapshot<Content> {
  sessionId: string;
  content: Content;
}

export interface VerifiedDraftCopy<Content> extends DraftSnapshot<Content> {}

export type ConflictTransitionChoice<Result> =
  | { kind: 'continue' }
  | { kind: 'cancelled'; result?: Result }
  | { kind: 'protect'; result: Result };

/**
 * Choose only after the caller has settled editors, waited for writes, frozen
 * editing, and captured the final snapshot. A matching independent copy lets
 * the explicit transition continue without another native reconciliation.
 */
export async function continueFromVerifiedCopyOrProtect<Content, Result>(
  snapshot: DraftSnapshot<Content>,
  copy: VerifiedDraftCopy<Content> | null,
  sameContent: (left: Content, right: Content) => boolean,
  isCurrent: () => boolean,
  protect: (snapshot: DraftSnapshot<Content>) => Promise<Result>,
): Promise<ConflictTransitionChoice<Result>> {
  if (!isCurrent()) return { kind: 'cancelled' };
  if (copy && copy.sessionId === snapshot.sessionId && sameContent(copy.content, snapshot.content)) {
    return { kind: 'continue' };
  }
  const result = await protect(snapshot);
  if (!isCurrent()) return { kind: 'cancelled', result };
  return { kind: 'protect', result };
}
