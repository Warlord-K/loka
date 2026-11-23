import { ArrowBendUpLeft, ArrowCounterClockwise, Crown, DotsThreeCircle, FlagCheckered, HourglassMedium, Lightbulb, Megaphone, Monitor, Robot } from "@phosphor-icons/react";

export const MeaningIcon = {
  agent: Robot,
  reply: ArrowBendUpLeft,
  tip: Lightbulb,
  suggestion: Lightbulb,
  otherAction: DotsThreeCircle,
  whatsNew: Megaphone,
  comingSoon: HourglassMedium,
  pro: Crown,
  result: FlagCheckered,
  tryAgain: ArrowCounterClockwise,
  watchBrowser: Monitor,
} as const;
