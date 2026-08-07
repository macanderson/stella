import "./globals.css";

export const metadata = {
  title: "ArenaBench",
  description: "Coding-agent benchmarks as a live, side-by-side contest.",
};

export default function RootLayout({ children }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
