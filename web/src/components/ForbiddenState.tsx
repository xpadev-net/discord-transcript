interface ForbiddenStateProps {
  message?: string;
}

export function ForbiddenState({ message }: ForbiddenStateProps) {
  return (
    <div className="panel-error forbidden-state" role="alert">
      <strong>{"\u8868\u793a\u3067\u304d\u307e\u305b\u3093"}</strong>
      <span>
        {message ??
          "\u3053\u306e\u30da\u30fc\u30b8\u3092\u8868\u793a\u3059\u308b\u6a29\u9650\u304c\u3042\u308a\u307e\u305b\u3093"}
      </span>
    </div>
  );
}
