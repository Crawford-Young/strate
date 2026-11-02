// Every box that scrolls, or clips positioned content, must be a containing block. An absolutely
// positioned descendant (the library's sr-only text: a transcript row's labels, a chart's data
// table) whose containing block sits above the box escapes its clip, so the page itself grew a
// scrollbar (#110 QA: html scrollHeight 12135 on a 900 px window). happy-dom has no layout, so this
// reads the classes; the live check is `documentElement.scrollHeight === innerHeight`.

const SCROLLS = /(?:^|\s)overflow(?:-[xy])?-(?:auto|scroll)(?=\s|$)/
const CLIPS = /(?:^|\s)overflow(?:-[xy])?-hidden(?=\s|$)/
const POSITIONED = /(?:^|\s)(?:relative|absolute|fixed|sticky)(?=\s|$)/
/** Content positioned out of flow: sr-only text and anything absolute. */
const OUT_OF_FLOW = '.sr-only, .absolute'

const classOf = (el: Element) => el.getAttribute('class') ?? ''

/**
 * The boxes under `root` that scroll their overflow, or clip it with positioned content inside
 * (a decorative bar that clips its own segments is neither).
 */
export const scrollBoxes = (root: Element) =>
  [...root.querySelectorAll('[class]')].filter(
    (el) =>
      SCROLLS.test(classOf(el)) ||
      (CLIPS.test(classOf(el)) && el.querySelector(OUT_OF_FLOW) !== null),
  )

/** The classes of each of `boxes` that is not positioned, so not a containing block. */
export const unpositioned = (boxes: Element[]) =>
  boxes.filter((el) => !POSITIONED.test(classOf(el))).map(classOf)
