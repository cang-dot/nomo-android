import MarkdownIt from 'markdown-it';
import type Token from 'markdown-it/lib/token.mjs';

export interface OutlineItem {
  id: string;
  level: 1 | 2 | 3 | 4 | 5 | 6;
  title: string;
  line: number;
}

export interface DocumentStats {
  chars: number;
  words: number;
  visibleChars: number;
  lines: number;
  headings: number;
  readingMinutes: number;
}

const writingStatsMarkdown = MarkdownIt('commonmark', { html: true }).enable([
  'table',
  'strikethrough',
]);

export function extractOutline(markdown: string): OutlineItem[] {
  return analyzeMarkdown(markdown).outline;
}

export function calculateDocumentStats(markdown: string): DocumentStats {
  return analyzeMarkdown(markdown).stats;
}

export function analyzeMarkdown(markdown: string): {
  outline: OutlineItem[];
  stats: DocumentStats;
} {
  if (markdown.length === 0) {
    return {
      outline: [],
      stats: {
        chars: 0,
        words: 0,
        visibleChars: 0,
        lines: 1,
        headings: 0,
        readingMinutes: 1,
      },
    };
  }

  const outline: OutlineItem[] = [];
  const usedIds = new Map<string, number>();
  const lines = markdown.split(/\r\n|\r|\n/);
  // 围栏代码块状态追踪：跳过代码块内的 # 标题匹配
  let inFence = false;
  let fenceMarker = '';

  lines.forEach((line, index) => {
    // 检测围栏代码块的开启与关闭（``` 或 ~~~）
    const fenceMatch = /^(\s*)(`{3,}|~{3,})/.exec(line);
    if (fenceMatch) {
      const marker = fenceMatch[2];
      if (!inFence) {
        inFence = true;
        fenceMarker = marker[0];
      } else if (marker[0] === fenceMarker) {
        inFence = false;
        fenceMarker = '';
      }
      return;
    }

    // 跳过代码块内的行，避免误识别为标题
    if (inFence) {
      return;
    }

    const match = /^(#{1,6})\s+(.+?)\s*#*\s*$/.exec(line);
    if (match) {
      const rawTitle = match[2].trim();
      const title = normalizeHeadingTitle(rawTitle) || rawTitle;
      const baseId = slugifyHeading(title) || `heading-${index + 1}`;
      const seen = usedIds.get(baseId) ?? 0;
      usedIds.set(baseId, seen + 1);

      outline.push({
        id: seen === 0 ? baseId : `${baseId}-${seen + 1}`,
        level: match[1].length as OutlineItem['level'],
        title,
        line: index + 1,
      });
    }
  });

  const withoutCode = markdown.replace(/```[\s\S]*?```/g, ' ');
  const words = withoutCode
    .replace(/[#>*_`[\]()!-]/g, ' ')
    .split(/[\s,.;:!?，。；：！？、]+/)
    .filter(Boolean).length;

  return {
    outline,
    stats: {
      chars: markdown.length,
      words,
      visibleChars: countVisibleCharacters(markdown),
      lines: lines.length,
      headings: outline.length,
      readingMinutes: Math.max(1, Math.ceil(words / 280)),
    },
  };
}

function countVisibleCharacters(markdown: string): number {
  const visibleText = extractVisibleMarkdownText(markdown);
  const Segmenter = (
    Intl as typeof Intl & {
      Segmenter?: new (
        locales?: string | string[],
        options?: { granularity: 'grapheme' },
      ) => { segment(input: string): Iterable<{ segment: string }> };
    }
  ).Segmenter;
  const segments =
    typeof Segmenter === 'function'
      ? Array.from(
          new Segmenter(undefined, { granularity: 'grapheme' }).segment(visibleText),
          ({ segment }) => segment,
        )
      : Array.from(visibleText);

  return segments.filter((segment) => !/^\s+$/u.test(segment)).length;
}

function extractVisibleMarkdownText(markdown: string): string {
  const visibleParts: string[] = [];
  appendVisibleTokenText(writingStatsMarkdown.parse(markdown, {}), visibleParts);
  return visibleParts.join('');
}

function appendVisibleTokenText(tokens: readonly Token[], visibleParts: string[]): void {
  for (const token of tokens) {
    if (token.type === 'image') {
      visibleParts.push(token.content);
      continue;
    }

    if (
      token.type === 'text' ||
      token.type === 'code_inline' ||
      token.type === 'code_block' ||
      token.type === 'fence'
    ) {
      visibleParts.push(token.content);
      continue;
    }

    if (token.type === 'softbreak' || token.type === 'hardbreak') {
      visibleParts.push('\n');
      continue;
    }

    if (token.type === 'html_inline' || token.type === 'html_block') {
      visibleParts.push(stripHtmlSyntax(token.content));
      continue;
    }

    if (token.children?.length) {
      appendVisibleTokenText(token.children, visibleParts);
    }
  }
}

function stripHtmlSyntax(html: string): string {
  return html.replace(/<!--[\s\S]*?-->/g, '').replace(/<[^>]*>/g, '');
}

function slugifyHeading(title: string): string {
  return title
    .toLowerCase()
    .replace(/[^\p{Letter}\p{Number}\s-]/gu, '')
    .trim()
    .replace(/\s+/g, '-');
}

export function normalizeHeadingTitle(title: string): string {
  let plain = title
    .replace(/!\[([^\]]*)\]\([^)]+\)/g, '$1')
    .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
    .replace(/`([^`]+)`/g, '$1');

  let previous = '';
  while (plain !== previous) {
    previous = plain;
    plain = plain
      .replace(
        /(^|[^\p{Letter}\p{Number}])(\*\*|__)(\S(?:[\s\S]*?\S)?)\2(?=$|[^\p{Letter}\p{Number}])/gu,
        '$1$3',
      )
      .replace(
        /(^|[^\p{Letter}\p{Number}])(\*|_)(\S(?:[\s\S]*?\S)?)\2(?=$|[^\p{Letter}\p{Number}])/gu,
        '$1$3',
      )
      .replace(
        /(^|[^\p{Letter}\p{Number}])~~(\S(?:[\s\S]*?\S)?)~~(?=$|[^\p{Letter}\p{Number}])/gu,
        '$1$2',
      );
  }

  return plain.replace(/\\([\\`*_[\]{}()#+\-.!>])/g, '$1').trim();
}
