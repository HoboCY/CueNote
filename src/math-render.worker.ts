import katex from 'katex';

interface RenderRequest {
  id: number;
  latex: string;
  displayMode: boolean;
}

const scope = globalThis as unknown as {
  onmessage: ((event: MessageEvent<RenderRequest>) => void) | null;
  postMessage: (message: unknown) => void;
};

scope.onmessage = ({ data }) => {
  try {
    let rejectedCommand = false;
    const html = katex.renderToString(data.latex, {
      displayMode: data.displayMode,
      throwOnError: true,
      strict: 'error',
      trust: () => {
        rejectedCommand = true;
        return false;
      },
      maxExpand: 256,
      maxSize: 20,
      // Definitions never leak between formulas or between notes.
      macros: {},
      globalGroup: false,
      output: 'htmlAndMathml',
    });
    if (rejectedCommand) {
      scope.postMessage({ id: data.id, html: null, error: '公式含不支持的外部资源或 HTML 命令；源码已保留，点击可修改。' });
    } else if (html.length > 131_072) {
      scope.postMessage({ id: data.id, html: null, error: '公式显示内容超出限制；源码已保留，可继续编辑和保存。' });
    } else {
      scope.postMessage({ id: data.id, html, error: null });
    }
  } catch {
    scope.postMessage({ id: data.id, html: null, error: '公式语法不受支持或超出排版限制；源码已保留，点击可修改。' });
  }
};

scope.postMessage({ ready: true });
