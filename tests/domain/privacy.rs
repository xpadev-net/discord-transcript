use discord_transcript::domain::privacy::mask_pii;

#[test]
fn mask_pii_replaces_mentions_email_and_phone() {
    let input = "Ping <@123456> and @alice. Mail: alice@example.com, phone: +1 (555) 123-4567.";
    let masked = mask_pii(input);

    assert!(masked.text.contains("[USER_1]"));
    assert!(masked.text.contains("[USER_2]"));
    assert!(masked.text.contains("[EMAIL_1]"));
    assert!(masked.text.contains("[PHONE_1]"));
    assert!(!masked.text.contains("<@123456>"));
    assert!(!masked.text.contains("@alice"));
    assert!(!masked.text.contains("alice@example.com"));
    assert!(!masked.text.contains("+1 (555) 123-4567"));
    assert_eq!(masked.stats.mention_replacements, 2);
    assert_eq!(masked.stats.email_replacements, 1);
    assert_eq!(masked.stats.phone_replacements, 1);
}

#[test]
fn mask_pii_uses_deterministic_token_for_same_value() {
    let input = "alice@example.com then alice@example.com";
    let masked = mask_pii(input);
    assert_eq!(masked.text, "[EMAIL_1] then [EMAIL_1]");
    assert_eq!(masked.stats.email_replacements, 2);
}
