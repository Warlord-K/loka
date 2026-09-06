import type { Metadata } from "next";
import { Pixelify_Sans } from "next/font/google";
import { WorldLoader } from "@/components/world/world-loader";

const pixel = Pixelify_Sans({
  subsets: ["latin"],
  weight: ["400", "500", "600"],
  variable: "--font-pixel",
  display: "swap",
});

export const metadata: Metadata = {
  title: { absolute: "Loka" },
  description: "Your town in Loka, where your agents live, work and head out on quests.",
  robots: { index: false, follow: false },
};

export default function WorldPage() {
  return (
    <div className={pixel.variable}>
      <WorldLoader />
    </div>
  );
}
