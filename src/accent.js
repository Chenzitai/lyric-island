/**
 * 从封面里取一个"能当主题色用"的颜色。
 *
 * 为什么不用简单平均 RGB：
 *   1. 直接平均会被大片黑/白像素拉成灰；
 *   2. 平均出来的颜色亮度不可控 —— 可能太暗（进度条看不见）或太亮（白色文字压不住）。
 *
 * 所以走 OKLCh：
 *   - Oklab 是感知均匀空间，距离和人眼感受基本一致，比 HSL 靠谱得多；
 *   - 取到主色后把亮度 L 夹到 [0.55, 0.78]，保证在深色底上既醒目又不刺眼；
 *   - 饱和度不够（黑白封面）就返回 null，让调用方回退到默认主题色。
 *
 * 降采样到 32×32 是有意的：一张 600×600 的封面逐像素扫是 36 万次，
 * 而 32×32 只有 1024 次 —— 视觉结果几乎一样，但不会卡住主线程。
 */

const SAMPLE = 32;

// 亮度夹取范围。低于 0.55，深色底上会糊；高于 0.78，白色文字压不住。
const L_MIN = 0.55;
const L_MAX = 0.78;
// 色度下限：低于这个值的像素视为"灰"，不参与主色统计。
// 阈值定得比"肉眼能看出是灰"更高一点：实测一张暖灰封面算出来 C≈0.04，
// 生成的颜色是 #98908f，看着像没换色，不如回退默认主题蓝。
const C_MIN = 0.07;

function srgbToLinear(c) {
  return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
}

function linearToSrgb(c) {
  return c <= 0.0031308 ? c * 12.92 : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
}

/** linear sRGB (0..1) → OKLab */
function linearToOklab(r, g, b) {
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return {
    L: 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    a: 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    b: 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  };
}

/** OKLab → linear sRGB（可能越界，调用方要 clamp） */
function oklabToLinear(L, a, b) {
  const l = L + 0.3963377774 * a + 0.2158037573 * b;
  const m = L - 0.1055613458 * a - 0.0638541728 * b;
  const s = L - 0.0894841775 * a - 1.291485548 * b;
  const l3 = l * l * l;
  const m3 = m * m * m;
  const s3 = s * s * s;
  return {
    r: 4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3,
    g: -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3,
    b: -0.0041960863 * l3 - 0.7034186147 * m3 + 1.707614701 * s3,
  };
}

function oklchToHex(L, C, h) {
  const a = C * Math.cos(h);
  const b = C * Math.sin(h);
  const lin = oklabToLinear(L, a, b);
  const to255 = (v) => {
    const s = linearToSrgb(Math.min(1, Math.max(0, v)));
    return Math.round(Math.min(1, Math.max(0, s)) * 255);
  };
  const r = to255(lin.r);
  const g = to255(lin.g);
  const bl = to255(lin.b);
  return `#${[r, g, bl].map((v) => v.toString(16).padStart(2, '0')).join('')}`;
}

/**
 * 从封面 data URL 提取主题色。
 *
 * @param {string} dataUrl 形如 `data:image/png;base64,...`
 * @returns {Promise<{accent:string, glow:string, text:string}|null>}
 *          取不到（加载失败、黑白封面、透明度太高）时返回 null
 */
export function extractAccent(dataUrl) {
  if (!dataUrl || !dataUrl.startsWith('data:image')) {
    return Promise.resolve(null);
  }

  return new Promise((resolve) => {
    const img = new Image();
    img.decoding = 'async';

    const timer = setTimeout(() => resolve(null), 2500);

    img.onerror = () => {
      clearTimeout(timer);
      resolve(null);
    };

    img.onload = () => {
      clearTimeout(timer);
      try {
        const canvas = document.createElement('canvas');
        canvas.width = SAMPLE;
        canvas.height = SAMPLE;
        const ctx = canvas.getContext('2d', { willReadFrequently: true });
        ctx.drawImage(img, 0, 0, SAMPLE, SAMPLE);
        const data = ctx.getImageData(0, 0, SAMPLE, SAMPLE).data;

        // 逐像素转到 OKLCh，只统计"有颜色"的部分
        let sumA = 0;
        let sumB = 0;
        let sumC = 0;
        let valid = 0;

        for (let i = 0; i < data.length; i += 4) {
          const alpha = data[i + 3];
          if (alpha < 160) continue;

          const r = srgbToLinear(data[i] / 255);
          const g = srgbToLinear(data[i + 1] / 255);
          const b = srgbToLinear(data[i + 2] / 255);
          const lab = linearToOklab(r, g, b);
          const C = Math.hypot(lab.a, lab.b);

          // 太暗或太亮的像素对"主色"贡献很小，跳过
          if (lab.L < 0.18 || lab.L > 0.92) continue;

          sumA += lab.a;
          sumB += lab.b;
          sumC += C;
          valid += 1;
        }

        if (!valid) {
          resolve(null);
          return;
        }

        const meanC = sumC / valid;
        // 平均色度太低 = 黑白 / 灰阶封面，交给默认主题色
        if (meanC < C_MIN) {
          resolve(null);
          return;
        }

        const a = sumA / valid;
        const b = sumB / valid;
        const C0 = Math.hypot(a, b);
        const h = Math.atan2(b, a);

        // 饱和度提一截：平均之后颜色总是偏灰，不提就出不来"主题色"的感觉
        const C = Math.min(0.22, C0 * 2.0);
        // 亮度夹到可用区间
        const L = Math.min(L_MAX, Math.max(L_MIN, 0.66));

        const accent = oklchToHex(L, C, h);
        // 光晕用同色但更淡
        const glow = oklchToHex(Math.min(0.9, L + 0.08), Math.min(0.2, C * 0.9), h);
        return resolve({
          accent,
          glow,
          text: accent,
        });
      } catch {
        resolve(null);
      }
    };

    img.src = dataUrl;
  });
}

/** 把取到的颜色写进 CSS 变量；传 null 时恢复默认。 */
export function applyAccent(colors) {
  const root = document.documentElement;
  if (!colors) {
    root.style.removeProperty('--accent');
    root.style.removeProperty('--accent-glow');
    root.style.removeProperty('--accent-text');
    return;
  }
  root.style.setProperty('--accent', colors.accent);
  root.style.setProperty('--accent-glow', hexToRgba(colors.glow, 0.6));
  root.style.setProperty('--accent-text', colors.text);
}

function hexToRgba(hex, alpha) {
  const h = hex.replace('#', '');
  const r = parseInt(h.slice(0, 2), 16);
  const g = parseInt(h.slice(2, 4), 16);
  const b = parseInt(h.slice(4, 6), 16);
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}
