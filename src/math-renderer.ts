export interface MathRenderResult {
  html: string | null;
  error: string | null;
}

const maxSourceLength = 16_384;
const maxDepth = 64;
const maxPending = 128;
const maxResultLength = 131_072;
const maxLiveResultLength = 2_097_152;

interface RenderJob {
  id: number;
  latex: string;
  displayMode: boolean;
  receive: (result: MathRenderResult) => void;
  cancelled: boolean;
  resultLength: number;
}

function sourceLimit(latex: string): string | null {
  if (latex.length > maxSourceLength) return '公式过长，暂不排版；源码已保留，可继续编辑和保存。';
  let depth = 0;
  for (let index = 0; index < latex.length; index += 1) {
    if (latex[index] === '\\') {
      index += 1;
    } else if (latex[index] === '{') {
      depth += 1;
      if (depth > maxDepth) return '公式嵌套过深，暂不排版；源码已保留，可继续编辑和保存。';
    } else if (latex[index] === '}') {
      depth = Math.max(0, depth - 1);
    }
  }
  return null;
}

/** One renderer per editor; presentation failure never changes its document. */
export class MathRenderer {
  private worker: Worker | null = null;
  private ready = false;
  private disposed = false;
  private nextId = 0;
  private queue: RenderJob[] = [];
  private active: RenderJob | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private liveResultLength = 0;

  render(latex: string, displayMode: boolean, receive: (result: MathRenderResult) => void): () => void {
    const error = sourceLimit(latex);
    if (this.disposed || error || this.queue.length + Number(Boolean(this.active)) >= maxPending) {
      receive({ html: null, error: error ?? '当前公式暂不排版；源码已保留，可继续编辑和保存。' });
      return () => {};
    }
    const job: RenderJob = { id: ++this.nextId, latex, displayMode, receive, cancelled: false, resultLength: 0 };
    this.queue.push(job);
    this.pump();
    return () => {
      if (job.cancelled) return;
      job.cancelled = true;
      this.liveResultLength -= job.resultLength;
      job.resultLength = 0;
      this.queue = this.queue.filter(pending => pending !== job);
      if (this.active === job) {
        this.active = null;
        this.stopWorker();
        this.pump();
      }
    };
  }

  dispose(): void {
    this.disposed = true;
    this.queue = [];
    this.active = null;
    this.stopWorker();
  }

  private clearTimer(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
  }

  private stopWorker(): void {
    this.clearTimer();
    this.worker?.terminate();
    this.worker = null;
    this.ready = false;
  }

  private complete(job: RenderJob, result: MathRenderResult): void {
    if (job.cancelled || this.disposed) return;
    if (result.html !== null) {
      if (result.html.length > maxResultLength || this.liveResultLength + result.html.length > maxLiveResultLength) {
        result = { html: null, error: '公式显示内容超出限制；源码已保留，可继续编辑和保存。' };
      } else {
        job.resultLength = result.html.length;
        this.liveResultLength += job.resultLength;
      }
    }
    job.receive(result);
  }

  private failWorker(message: string): void {
    const active = this.active;
    this.active = null;
    this.stopWorker();
    if (active) {
      this.complete(active, { html: null, error: message });
      this.pump();
    } else {
      // A startup failure affects this batch; do not endlessly restart workers.
      const waiting = this.queue.splice(0);
      for (const job of waiting) this.complete(job, { html: null, error: message });
    }
  }

  private pump(): void {
    if (this.disposed || this.active || this.queue.length === 0) return;
    if (!this.worker) {
      try {
        const worker = new Worker(new URL('./math-render.worker.ts', import.meta.url), { type: 'module' });
        this.worker = worker;
        worker.onmessage = (event: MessageEvent) => {
          if (worker !== this.worker) return;
          if (event.data?.ready === true) {
            this.clearTimer();
            this.ready = true;
            this.pump();
            return;
          }
          const job = this.active;
          if (!job || event.data?.id !== job.id) return;
          this.clearTimer();
          this.active = null;
          const html = typeof event.data.html === 'string' ? event.data.html : null;
          const error = typeof event.data.error === 'string' ? event.data.error.slice(0, 240) : null;
          this.complete(job, { html, error: html === null ? error ?? '无法显示公式；源码已保留。' : null });
          this.pump();
        };
        worker.onerror = event => {
          event.preventDefault();
          if (worker === this.worker) this.failWorker('公式显示暂不可用；源码已保留，可继续编辑和保存。');
        };
        worker.onmessageerror = () => {
          if (worker === this.worker) this.failWorker('公式显示暂不可用；源码已保留，可继续编辑和保存。');
        };
        this.timer = setTimeout(() => this.failWorker('公式显示启动超时；源码已保留，可继续编辑和保存。'), 3_000);
      } catch {
        this.failWorker('公式显示暂不可用；源码已保留，可继续编辑和保存。');
      }
      return;
    }
    if (!this.ready) return;
    const job = this.queue.shift();
    if (!job) return;
    this.active = job;
    this.timer = setTimeout(() => this.failWorker('公式排版超时；源码已保留，可继续编辑和保存。'), 500);
    try {
      this.worker.postMessage({ id: job.id, latex: job.latex, displayMode: job.displayMode });
    } catch {
      this.failWorker('公式显示暂不可用；源码已保留，可继续编辑和保存。');
    }
  }
}
