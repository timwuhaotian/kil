import React from 'react';
import { useCurrentFrame } from 'remotion';
import { C, MONO, SANS } from './theme';

export type TabInfo = {
  name: string;
  dir: string;
  state?: 'running' | 'done';
  mem?: string;
};

export type Line = { t: string; c?: string };

/** The Kil app chrome: vertical tabs on the left, terminal on the right. */
export const KilApp: React.FC<{
  width?: number;
  height?: number;
  tabs: TabInfo[];
  active: number;
  panes?: Line[][];
  split?: number; // width share of the first pane, 0..1
  fontSize?: number;
  title?: string;
  radius?: number;
  shadow?: boolean;
  paneTop?: number;
  cursor?: boolean;
  footer?: string;
  paneAlign?: 'top' | 'center';
}> = ({
  width = 1920,
  height = 1080,
  tabs,
  active,
  panes = [],
  split = 0.58,
  fontSize = 25,
  title,
  radius = 0,
  shadow = false,
  paneTop = 26,
  cursor = false,
  footer,
  paneAlign = 'top',
}) => {
  const frame = useCurrentFrame();
  const sidebar = Math.round(Math.min(width * 0.19, 360));
  const cols = panes.length === 2 ? [split, 1 - split] : [1];
  const showNewTab = sidebar >= 150;
  const showDir = sidebar >= 150;
  const dirSize = sidebar >= 300 ? 18 : Math.round(fontSize * 0.58);
  const blink = Math.floor(frame / 12) % 2 === 0;

  return (
    <div
      style={{
        width,
        height,
        display: 'flex',
        flexDirection: 'column',
        background: C.term,
        borderRadius: radius,
        overflow: 'hidden',
        fontFamily: SANS,
        color: C.text,
        boxShadow: shadow ? '0 60px 140px rgba(0,0,0,0.65)' : undefined,
        border: radius ? '1px solid #2b2f36' : undefined,
      }}
    >
      <div
        style={{
          height: Math.round(height * 0.055),
          minHeight: 54,
          background: 'rgba(255,255,255,0.025)',
          borderBottom: `1px solid ${C.divider}`,
          display: 'flex',
          alignItems: 'center',
          padding: '0 22px',
          gap: 10,
          flexShrink: 0,
        }}
      >
        {['#ff5f57', '#febc2e', '#28c840'].map((c) => (
          <div key={c} style={{ width: 14, height: 14, borderRadius: 7, background: c }} />
        ))}
        {title ? (
          <div
            style={{
              marginLeft: 18,
              fontSize: Math.round(fontSize * 0.62),
              color: C.muted,
              fontFamily: MONO,
            }}
          >
            {title}
          </div>
        ) : null}
      </div>

      <div style={{ flex: 1, display: 'flex', minHeight: 0 }}>
        <div
          style={{
            width: sidebar,
            background: C.sidebar,
            borderRight: `1px solid ${C.divider}`,
            display: 'flex',
            flexDirection: 'column',
            padding: '22px 14px 18px',
            gap: 8,
          }}
        >
          {tabs.map((tab, i) => (
            <div
              key={tab.name}
              style={{
                background: i === active ? 'rgba(255,255,255,0.055)' : 'transparent',
                borderRadius: 10,
                padding: '10px 12px',
                display: 'flex',
                flexDirection: 'column',
                gap: 4,
              }}
            >
              <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
                {tab.state === 'running' ? (
                  <div
                    style={{
                      width: 13,
                      height: 13,
                      borderRadius: 7,
                      border: `2px solid ${C.accent}`,
                      flexShrink: 0,
                    }}
                  />
                ) : tab.state === 'done' ? (
                  <div
                    style={{
                      width: 11,
                      height: 11,
                      borderRadius: 6,
                      background: C.accent,
                      flexShrink: 0,
                    }}
                  />
                ) : (
                  <div style={{ width: 13, flexShrink: 0 }} />
                )}
                <div
                  style={{
                    fontSize: Math.round(fontSize * 0.78),
                    fontWeight: 600,
                    letterSpacing: 0.2,
                    color: i === active ? C.text : C.muted,
                  }}
                >
                  {tab.name}
                </div>
                {tab.mem ? (
                  <div
                    style={{
                      marginLeft: 'auto',
                      fontSize: Math.round(fontSize * 0.68),
                      color: C.muted,
                      fontFamily: MONO,
                      whiteSpace: 'nowrap',
                    }}
                  >
                    {tab.mem}
                  </div>
                ) : null}
              </div>
              {showDir ? (
                <div
                  style={{
                    fontSize: dirSize,
                    color: C.muted,
                    fontFamily: MONO,
                    paddingLeft: 16,
                    overflow: 'hidden',
                    textOverflow: 'ellipsis',
                    whiteSpace: 'nowrap',
                  }}
                >
                  {tab.dir}
                </div>
              ) : null}
            </div>
          ))}
          {footer ? (
            <div
              style={{
                marginTop: 'auto',
                paddingLeft: 12,
                fontSize: Math.round(fontSize * 0.66),
                color: C.muted,
                fontFamily: MONO,
                whiteSpace: 'nowrap',
              }}
            >
              {footer}
            </div>
          ) : null}
          {showNewTab ? (
            <div
              style={{
                marginTop: footer ? 8 : 'auto',
                fontSize: Math.round(fontSize * 0.66),
                color: C.muted,
                paddingLeft: 12,
                whiteSpace: 'nowrap',
              }}
            >
              + New Tab
            </div>
          ) : null}
        </div>

        <div style={{ flex: 1, display: 'flex', minWidth: 0 }}>
          {panes.map((pane, i) => {
            const isLast = i === panes.length - 1;
            return (
              <div
                key={i}
                style={{
                  flex: `0 0 ${cols[i] * 100}%`,
                  minWidth: 0,
                  overflow: 'hidden',
                  display: 'flex',
                  borderRight: isLast ? undefined : `1px solid ${C.divider}`,
                }}
              >
                <div
                  style={{
                    flex: 1,
                    minWidth: 0,
                    display: 'flex',
                    flexDirection: 'column',
                    justifyContent: paneAlign === 'center' ? 'center' : 'flex-start',
                    padding: `${paneTop}px 30px 26px 30px`,
                    fontFamily: MONO,
                    fontSize,
                    lineHeight: 1.5,
                    whiteSpace: 'pre',
                    maskImage:
                      'linear-gradient(90deg, rgba(0,0,0,1) 88%, rgba(0,0,0,0) 100%)',
                    WebkitMaskImage:
                      'linear-gradient(90deg, rgba(0,0,0,1) 88%, rgba(0,0,0,0) 100%)',
                  }}
                >
                  {pane.map((line, j) => (
                    <div key={j} style={{ color: line.c ?? C.text }}>
                      {line.t}
                      {cursor && isLast && j === pane.length - 1 ? (
                        <span
                          style={{
                            display: 'inline-block',
                            width: Math.round(fontSize * 0.58),
                            height: fontSize * 1.02,
                            marginLeft: 2,
                            background: blink ? '#c3c8d1' : 'transparent',
                            verticalAlign: 'text-bottom',
                          }}
                        />
                      ) : null}
                    </div>
                  ))}
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
};

/** A stock macOS Terminal window — the "before". */
export const AppleTerminal: React.FC<{
  width?: number;
  height?: number;
  lines: Line[];
  fontSize?: number;
}> = ({ width = 900, height = 520, lines, fontSize = 22 }) => (
  <div
    style={{
      width,
      height,
      borderRadius: 12,
      overflow: 'hidden',
      background: '#ffffff',
      color: '#1c1c1e',
      boxShadow: '0 40px 110px rgba(0,0,0,0.6)',
      border: '1px solid rgba(255,255,255,0.25)',
      fontFamily: SANS,
    }}
  >
    <div
      style={{
        height: 38,
        background: 'linear-gradient(#e6e6e8,#d6d6d9)',
        borderBottom: '1px solid #c2c2c6',
        display: 'flex',
        alignItems: 'center',
        padding: '0 14px',
        gap: 8,
        position: 'relative',
      }}
    >
      {['#ff5f57', '#febc2e', '#28c840'].map((c) => (
        <div key={c} style={{ width: 12, height: 12, borderRadius: 6, background: c }} />
      ))}
      <div
        style={{
          position: 'absolute',
          left: 0,
          right: 0,
          textAlign: 'center',
          fontSize: 13,
          color: '#6b6b70',
          pointerEvents: 'none',
        }}
      >
        Terminal
      </div>
    </div>
    <div
      style={{
        padding: '20px 24px',
        fontFamily: MONO,
        fontSize,
        lineHeight: 1.55,
        whiteSpace: 'pre',
      }}
    >
      {lines.map((l, i) => (
        <div key={i} style={{ color: l.c ?? '#1c1c1e' }}>
          {l.t}
        </div>
      ))}
    </div>
  </div>
);

/** Big headline + subline, used as the on-screen copy of every scene. */
export const Headline: React.FC<{
  title: React.ReactNode;
  sub?: string;
  size?: number;
  align?: 'left' | 'center';
  color?: string;
  accent?: string;
}> = ({ title, sub, size = 62, align = 'left', color = C.text, accent = C.accent }) => (
  <div
    style={{
      fontFamily: SANS,
      textAlign: align,
      display: 'flex',
      flexDirection: 'column',
      gap: 16,
      alignItems: align === 'center' ? 'center' : 'flex-start',
    }}
  >
    <div
      style={{
        fontSize: size,
        fontWeight: 700,
        letterSpacing: -1.6,
        lineHeight: 1.08,
        color,
      }}
    >
      {title}
    </div>
    {sub ? (
      <div style={{ fontSize: 30, color: C.muted, letterSpacing: -0.4 }}>{sub}</div>
    ) : null}
  </div>
);

/** Bottom caption card, the burned-in subtitle of the video. */
export const Caption: React.FC<{
  children: React.ReactNode;
  bottom?: number;
  left?: number;
  size?: number;
}> = ({ children, bottom = 84, left = 390, size = 34 }) => (
  <div
    style={{
      position: 'absolute',
      left,
      bottom,
      maxWidth: 1180,
      width: 'fit-content',
      display: 'flex',
      alignItems: 'center',
      gap: 20,
      padding: '22px 30px 22px 22px',
      borderRadius: 16,
      background: 'rgba(18,21,26,0.92)',
      border: '1px solid rgba(108,168,255,0.28)',
      fontSize: size,
      color: C.text,
      fontFamily: SANS,
      lineHeight: 1.25,
      letterSpacing: -0.4,
    }}
  >
    <div
      style={{
        width: 6,
        alignSelf: 'stretch',
        borderRadius: 3,
        background: C.accent,
        flexShrink: 0,
      }}
    />
    {children}
  </div>
);

/** A floating status pill. Video-layer chrome, not part of the app mock. */
export const Pill: React.FC<{
  text: string;
  color?: string;
  style?: React.CSSProperties;
}> = ({ text, color = C.success, style }) => (
  <div
    style={{
      position: 'absolute',
      display: 'flex',
      alignItems: 'center',
      gap: 12,
      padding: '12px 24px',
      borderRadius: 999,
      background: 'rgba(20,24,28,0.92)',
      border: `1px solid ${color}80`,
      color,
      fontSize: 30,
      fontWeight: 600,
      fontFamily: SANS,
      whiteSpace: 'nowrap',
      boxShadow: '0 18px 50px rgba(0,0,0,0.55)',
      ...style,
    }}
  >
    <div style={{ width: 13, height: 13, borderRadius: 7, background: color }} />
    {text}
  </div>
);

/** A key cap, e.g. ⌘⇧D. */
export const Kbd: React.FC<{ children: React.ReactNode }> = ({ children }) => (
  <span
    style={{
      display: 'inline-block',
      fontSize: '0.82em',
      fontFamily: MONO,
      padding: '0.16em 0.42em',
      margin: '0 0.16em',
      borderRadius: 6,
      background: 'rgba(108,168,255,0.16)',
      border: '1px solid rgba(108,168,255,0.36)',
      color: C.accent,
      lineHeight: 1.25,
    }}
  >
    {children}
  </span>
);
