import React from "react";

interface Props {
  title: string;
}

/** Renders a heading. */
export function Heading({ title }: Props) {
  return <h1>{title}</h1>;
}

export const Badge = (props: { count: number }) => <span>{props.count}</span>;

class Store {
  private items: string[] = [];
  add(item: string): void {
    this.items.push(item);
  }
}
